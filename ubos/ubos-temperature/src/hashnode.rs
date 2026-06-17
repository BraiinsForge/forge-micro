//! Hashnode board temperature aggregator.
//!
//! Combines a TMP461 remote-diode sensor for the ASIC die with two TMP102
//! PCB sensors and exposes two temperatures to the thermal layer. The real
//! die temperature from the TMP461 remote channel gates thermal shutdown,
//! published as `board_chip_temperature_millicelsius{source="chip"}`. A
//! PCB-plus-offset estimate from the hottest TMP102 drives the cooling PID,
//! published as `board_chip_temperature_millicelsius{source="approximated"}`.
//! When the remote diode read fails, the estimate also becomes the safety
//! value so thermal protection is never blind.
//!
//! The TMP461 local channel and both TMP102 sensors are published as
//! `board_pcb_temperature_millicelsius` for board-surface visibility.
//!
//! Note on `dangerous_temp_c`: the 90 °C runtime default in `ThermalConfig`
//! is calibrated for PCB-surface sensors. Because the safety gate now reads
//! real die temperature, the Hashnode threshold should be raised toward
//! ~108 °C; override it in the per-board `ThermalConfig`, not in the shared
//! `ubos-config` default.

use core::fmt::Write as _;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::Temperature;
use ubos_logging::error;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

use crate::TemperatureSensor;
use crate::tmp102::Tmp102;
use crate::tmp461::Tmp461;

/// I2C address of the TMP461 on the Hashnode board.
/// The remote-diode input is wired to the ASIC die.
const DIE_ADDRESS: u8 = 0x4E;

/// I2C addresses of the two TMP102 sensors on the Hashnode PCB.
const PCB_ADDRESSES: [u8; 2] = [0x48, 0x49];

/// Added to the hottest PCB reading to estimate the ASIC die temperature
/// from a surface measurement. Drives the cooling PID, and becomes the
/// safety value when the real die reading is unavailable.
const PCB_TO_DIE_OFFSET_MILLICELSIUS: i32 = 15_000;

/// Hashnode board temperature aggregator: one TMP461 remote-diode sensor
/// for the ASIC die and two TMP102 sensors for PCB-surface reference.
/// Instantiated by the hashnode firmware crate.
pub struct HashnodeTemperature<Bus> {
    bus: Bus,
    die: Tmp461,
    pcb: [Tmp102; 2],
    /// PCB-plus-offset estimate cached from the last `read`, exposed to the
    /// cooling PID. `None` when the real die read failed and the estimate
    /// became the safety value, so no separate series is published.
    pid_temp: Option<Temperature>,
    /// Source label for the last `read` safety value: `"chip"` for the real
    /// die, `"approximated"` for the PCB-estimate fallback.
    safety_source: Option<&'static str>,
}

impl<Bus> HashnodeTemperature<Bus> {
    /// Construct the aggregator for the Hashnode board using the supplied
    /// shared I2C bus handle.
    #[must_use]
    pub fn new(bus: Bus) -> Self {
        Self {
            bus,
            die: Tmp461::new(DIE_ADDRESS),
            pcb: PCB_ADDRESSES.map(Tmp102::new),
            pid_temp: None,
            safety_source: None,
        }
    }
}

impl<Bus> TemperatureSensor for HashnodeTemperature<Bus>
where
    Bus: I2c,
{
    async fn read(&mut self) -> Option<Temperature> {
        // Real ASIC die temperature from the TMP461 remote diode, the
        // authoritative reading for thermal shutdown when valid.
        let remote = self.die.read_remote_millicelsius(&mut self.bus).await;

        // The TMP461 local channel measures the PCB at the sensor's own
        // location, so publish it as a board_pcb_temperature reading like
        // the TMP102 sensors.
        match self.die.read_local_millicelsius(&mut self.bus).await {
            Ok(millicelsius) => publish_pcb_temperature(self.die.address(), millicelsius).await,
            Err(error) => log_local_fault(&error),
        }

        // Read both TMP102 sensors regardless of the die outcome so their
        // metrics are always available, and keep the hottest for the
        // estimate.
        let mut hottest_pcb: Option<i32> = None;
        for sensor in &self.pcb {
            match sensor.read_millicelsius(&mut self.bus).await {
                Ok(reading) => {
                    hottest_pcb = Some(hottest_pcb.map_or(reading, |current| current.max(reading)));
                    publish_pcb_temperature(sensor.address(), reading).await;
                }
                Err(crate::tmp102::Error::I2c(bus_error)) => {
                    error!(
                        "temperature: TMP102 {:#x} I2C error: {:?}",
                        sensor.address(),
                        ubos_logging::Debug2Format(&bus_error),
                    );
                }
                Err(crate::tmp102::Error::OutOfRange(millicelsius)) => {
                    error!(
                        "temperature: TMP102 {:#x} reading {} mC out of range",
                        sensor.address(),
                        millicelsius,
                    );
                }
            }
        }

        let synthetic = hottest_pcb.map(|millicelsius| {
            Temperature::from_millicelsius(millicelsius + PCB_TO_DIE_OFFSET_MILLICELSIUS)
        });

        match remote {
            Ok(millicelsius) => {
                self.pid_temp = synthetic;
                self.safety_source = Some(crate::CHIP_SOURCE);
                Some(Temperature::from_millicelsius(millicelsius))
            }
            Err(error) => {
                log_remote_fault(&error);
                // No trustworthy die reading: the estimate becomes the
                // safety value, published as the single approximated series.
                self.pid_temp = None;
                self.safety_source = synthetic.map(|_| "approximated");
                synthetic
            }
        }
    }

    fn pid_temperature(&self) -> Option<Temperature> {
        self.pid_temp
    }

    fn temperature_source(&self) -> Option<&'static str> {
        self.safety_source
    }

    fn pid_temperature_source(&self) -> Option<&'static str> {
        self.pid_temp.map(|_| "approximated")
    }
}

/// Push a PCB-surface reading as a `board_pcb_temperature` gauge tagged with
/// the sensor's I2C address.
async fn publish_pcb_temperature(address: u8, millicelsius: i32) {
    let mut address_str = heapless::String::<24>::new();
    let _ = write!(address_str, "{address:#04x}");
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "addr",
        value: LabelValue::Short(address_str),
    });
    push(Metric::with_labels(
        "board_pcb_temperature_millicelsius",
        labels,
        Value::Int(i64::from(millicelsius)),
    ))
    .await;
}

/// Log a TMP461 local-channel read fault. The local channel only ever
/// returns a bus error or an out-of-range decode; the remote-diode faults
/// cannot occur here.
fn log_local_fault<E: core::fmt::Debug>(error: &crate::tmp461::Error<E>) {
    match error {
        crate::tmp461::Error::OutOfRange(millicelsius) => error!(
            "temperature: TMP461 {:#x} local reading {} mC out of range",
            DIE_ADDRESS, millicelsius,
        ),
        crate::tmp461::Error::I2c(bus_error) => error!(
            "temperature: TMP461 {:#x} I2C error: {:?}",
            DIE_ADDRESS,
            ubos_logging::Debug2Format(bus_error),
        ),
        crate::tmp461::Error::OpenCircuit
        | crate::tmp461::Error::ShortCircuit
        | crate::tmp461::Error::Saturated => {}
    }
}

/// Log a TMP461 remote-diode read fault. `OpenCircuit` is expected while the
/// ASIC is unpowered; the rest point to a wiring or sensor problem.
fn log_remote_fault<E: core::fmt::Debug>(error: &crate::tmp461::Error<E>) {
    let reason = match error {
        crate::tmp461::Error::OpenCircuit => "remote diode open-circuit",
        crate::tmp461::Error::ShortCircuit => "remote diode short-circuit",
        crate::tmp461::Error::Saturated => "remote diode floating or railed",
        crate::tmp461::Error::OutOfRange(millicelsius) => {
            error!(
                "temperature: TMP461 {:#x} remote reading {} mC out of range",
                DIE_ADDRESS, millicelsius,
            );
            return;
        }
        crate::tmp461::Error::I2c(bus_error) => {
            error!(
                "temperature: TMP461 {:#x} I2C error: {:?}",
                DIE_ADDRESS,
                ubos_logging::Debug2Format(bus_error),
            );
            return;
        }
    };
    error!("temperature: TMP461 {:#x} {}", DIE_ADDRESS, reason);
}
