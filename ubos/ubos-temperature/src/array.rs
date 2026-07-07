//! Descriptor-driven multi-sensor hashboard temperature aggregator.
//!
//! A board describes its sensor topology as a `&'static [SensorDescriptor]`
//! table, then instantiates [`SensorArray`] with that table. On each sample,
//! `SensorArray` reads every sensor, folds the readings into the two values
//! the thermal layer needs — a safety reading (real die temperature when a
//! sensor's remote diode is wired to a chip, else a PCB-plus-offset estimate)
//! and a cooling-PID estimate — and publishes per-sensor metrics for sensors
//! that have a named gauge. Boards supply only the topology; all reading and
//! folding logic lives here.

use core::fmt::Write as _;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::Temperature;
use ubos_logging::error;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

use crate::TemperatureSensor;

use crate::lm75a::{self, Lm75a};
use crate::tmp102::{self, Tmp102};
use crate::tmp461::{self, Tmp461};

#[cfg(test)]
#[path = "tests/array.rs"]
mod tests;

const APPROXIMATED_SOURCE: &str = "approximated";

/// Which chip driver backs a sensor entry.
#[derive(Clone, Copy)]
pub enum SensorKind {
    Lm75a,
    Tmp102,
    Tmp461,
}

impl SensorKind {
    /// Per-sensor gauge name, or `None` to publish no per-sensor metric.
    const fn metric_name(self) -> Option<&'static str> {
        match self {
            SensorKind::Lm75a => Some("board_lm75a_temperature_millicelsius"),
            SensorKind::Tmp102 | SensorKind::Tmp461 => None,
        }
    }
}

/// One sensor's board topology, supplied by the board as const data.
#[derive(Clone, Copy)]
pub struct SensorDescriptor {
    pub kind: SensorKind,
    pub address: u8,
    /// Added to a PCB sensor's reading to approximate die temperature.
    pub pcb_to_die_offset_mc: i32,
    /// Chip index whose remote diode this sensor reads, if any. `Some`
    /// marks the sensor's remote reading as a trustworthy die temperature;
    /// `None` means a PCB-surface sensor. A plain index for now — nothing
    /// consumes the value yet.
    pub attached_chip: Option<u8>,
}

/// A sensor's local (own body/PCB) and optional remote (die diode)
/// readings in millicelsius.
struct DualReading {
    local: Option<i32>,
    remote: Option<i32>,
}

/// Fused board temperatures for one sample.
struct Fused {
    safety: Option<Temperature>,
    safety_source: Option<&'static str>,
    pid: Option<Temperature>,
}

/// Pure fold. `die` = hottest trustworthy die reading, `est` = hottest
/// PCB-plus-offset estimate, `has_die_capable` = the table contains at
/// least one die-wired sensor. Picks the safety value (real die if any,
/// else the estimate), tags its source, and exposes the estimate as the
/// PID input only when it is a distinct second series.
fn fuse(die: Option<i32>, est: Option<i32>, has_die_capable: bool) -> Fused {
    match die {
        Some(mc) => Fused {
            safety: Some(Temperature::from_millicelsius(mc)),
            safety_source: Some(crate::CHIP_SOURCE),
            pid: est.map(Temperature::from_millicelsius),
        },
        None => Fused {
            safety: est.map(Temperature::from_millicelsius),
            // "approximated" only when there is a real die sensor to
            // contrast against; a board with no die sensor publishes the
            // canonical unlabeled gauge instead.
            safety_source: if has_die_capable && est.is_some() {
                Some(APPROXIMATED_SOURCE)
            } else {
                None
            },
            pid: None,
        },
    }
}

/// A sensor fault normalized across every driver's `Error` enum, so all
/// fault logging lives in one place. Each driver's error maps in via
/// `From`; the drivers themselves stay pure `Result`-returning wrappers.
enum SensorFault<E> {
    I2c(E),
    OutOfRange(i32),
    OpenCircuit,
    ShortCircuit,
    Saturated,
}

impl<E> From<lm75a::Error<E>> for SensorFault<E> {
    fn from(e: lm75a::Error<E>) -> Self {
        match e {
            lm75a::Error::I2c(e) => Self::I2c(e),
            lm75a::Error::OutOfRange(mc) => Self::OutOfRange(mc),
        }
    }
}

impl<E> From<tmp102::Error<E>> for SensorFault<E> {
    fn from(e: tmp102::Error<E>) -> Self {
        match e {
            tmp102::Error::I2c(e) => Self::I2c(e),
            tmp102::Error::OutOfRange(mc) => Self::OutOfRange(mc),
        }
    }
}

impl<E> From<tmp461::Error<E>> for SensorFault<E> {
    fn from(e: tmp461::Error<E>) -> Self {
        match e {
            tmp461::Error::I2c(e) => Self::I2c(e),
            tmp461::Error::OutOfRange(mc) => Self::OutOfRange(mc),
            tmp461::Error::OpenCircuit => Self::OpenCircuit,
            tmp461::Error::ShortCircuit => Self::ShortCircuit,
            tmp461::Error::Saturated => Self::Saturated,
        }
    }
}

/// Log a fault. `channel` qualifies the out-of-range message for
/// multi-channel sensors and is empty for single-channel ones.
fn log_fault<E: core::fmt::Debug>(chip: &str, address: u8, channel: &str, fault: SensorFault<E>) {
    // The diode faults share one message shape, so they fold to a reason
    // string and log once; I2C and out-of-range have their own shapes.
    let diode = match fault {
        SensorFault::I2c(e) => {
            error!(
                "temperature: {} {:#x} I2C error: {:?}",
                chip,
                address,
                ubos_logging::Debug2Format(&e),
            );
            return;
        }
        SensorFault::OutOfRange(mc) => {
            error!(
                "temperature: {} {:#x}{} reading {} mC out of range",
                chip, address, channel, mc,
            );
            return;
        }
        SensorFault::OpenCircuit => "open-circuit",
        SensorFault::ShortCircuit => "short-circuit",
        SensorFault::Saturated => "floating or railed",
    };
    error!(
        "temperature: {} {:#x} remote diode {}",
        chip, address, diode
    );
}

/// Take one channel reading: pass the value through, or log the fault and
/// yield `None`.
fn take<E: core::fmt::Debug>(
    chip: &str,
    address: u8,
    channel: &str,
    reading: Result<i32, SensorFault<E>>,
) -> Option<i32> {
    match reading {
        Ok(mc) => Some(mc),
        Err(fault) => {
            log_fault(chip, address, channel, fault);
            None
        }
    }
}

/// Read one sensor, dispatching on kind to name its concrete driver type
/// and channels. All fault logging is funneled through `take`.
async fn read_sensor<Bus: I2c>(desc: &SensorDescriptor, bus: &mut Bus) -> DualReading {
    let addr = desc.address;
    match desc.kind {
        SensorKind::Lm75a => DualReading {
            local: take(
                "LM75A",
                addr,
                "",
                Lm75a::new(addr)
                    .read_millicelsius(bus)
                    .await
                    .map_err(SensorFault::from),
            ),
            remote: None,
        },
        SensorKind::Tmp102 => DualReading {
            local: take(
                "TMP102",
                addr,
                "",
                Tmp102::new(addr)
                    .read_millicelsius(bus)
                    .await
                    .map_err(SensorFault::from),
            ),
            remote: None,
        },
        SensorKind::Tmp461 => {
            let sensor = Tmp461::new(addr);
            DualReading {
                remote: take(
                    "TMP461",
                    addr,
                    " remote",
                    sensor
                        .read_remote_millicelsius(bus)
                        .await
                        .map_err(SensorFault::from),
                ),
                local: take(
                    "TMP461",
                    addr,
                    " local",
                    sensor
                        .read_local_millicelsius(bus)
                        .await
                        .map_err(SensorFault::from),
                ),
            }
        }
    }
}

/// Publish a per-sensor metric labeled by I2C address.
async fn publish_sensor(name: &'static str, address: u8, millicelsius: i32) {
    let mut address_str = heapless::String::<24>::new();
    let _ = write!(address_str, "{address:#04x}");
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "addr",
        value: LabelValue::Short(address_str),
    });
    push(Metric::with_labels(
        name,
        labels,
        Value::Int(i64::from(millicelsius)),
    ))
    .await;
}

/// Multi-sensor hashboard aggregator backed by a descriptor slice.
pub struct SensorArray<Bus> {
    bus: Bus,
    sensors: &'static [SensorDescriptor],
    pid_temp: Option<Temperature>,
    safety_source: Option<&'static str>,
}

impl<Bus> SensorArray<Bus> {
    #[must_use]
    pub const fn new(bus: Bus, sensors: &'static [SensorDescriptor]) -> Self {
        Self {
            bus,
            sensors,
            pid_temp: None,
            safety_source: None,
        }
    }
}

impl<Bus> TemperatureSensor for SensorArray<Bus>
where
    Bus: I2c,
{
    async fn read(&mut self) -> Option<Temperature> {
        let mut die: Option<i32> = None;
        let mut est: Option<i32> = None;
        let mut has_die_capable = false;

        for desc in self.sensors {
            let reading = read_sensor(desc, &mut self.bus).await;
            if desc.attached_chip.is_some() {
                has_die_capable = true;
                // Die-wired sensor: only its remote diode feeds the safety
                // value. The local channel is read only so its I2C faults are
                // logged; the value is not a board PCB reference, so it is
                // neither used nor folded into the estimate.
                if let Some(mc) = reading.remote {
                    die = Some(die.map_or(mc, |cur| cur.max(mc)));
                }
            } else if let Some(local) = reading.local {
                let adjusted = local + desc.pcb_to_die_offset_mc;
                est = Some(est.map_or(adjusted, |cur| cur.max(adjusted)));
                if let Some(name) = desc.kind.metric_name() {
                    publish_sensor(name, desc.address, adjusted).await;
                }
            }
        }

        let fused = fuse(die, est, has_die_capable);
        self.pid_temp = fused.pid;
        self.safety_source = fused.safety_source;
        fused.safety
    }

    fn pid_temperature(&self) -> Option<Temperature> {
        self.pid_temp
    }

    fn temperature_source(&self) -> Option<&'static str> {
        self.safety_source
    }

    fn pid_temperature_source(&self) -> Option<&'static str> {
        self.pid_temp.map(|_| APPROXIMATED_SOURCE)
    }
}
