// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Async PSU driver for the BMM101 miniminer hashboard.
//!
//! Generic over `embedded_hal_async::i2c::I2c`. Runs unchanged on
//! Linux (via `embedded-i2c-unix`) and on rp2040/rp2350 (via
//! `embassy-rp::i2c`).
//!
//! The metrics surface below is free functions over private statics,
//! not methods on a type. There is one metrics store per firmware
//! image, so a handle owning it would promise an instance no caller can
//! choose. [`handle`] returns a `Handle` that is `Copy` and carries no
//! state, which makes it a token rather than an owner. Six other crates
//! in this tree expose the same `handle()` shape for the same reason.

#![cfg_attr(not(test), no_std)]
// Embassy executors do not require `Send`; adding the bound to
// every trait method's future would force every I2C impl and all
// internal state to be `Send`, which is not free on no_std.
#![allow(async_fn_in_trait)]
// Hardware register getters are intentionally side-effect-free
// returns; #[must_use] sprinkled across every accessor is noise.
#![allow(clippy::must_use_candidate)]
// Bounded casts inside hardware drivers (DAC byte, register
// pointer) are safe by construction; the math above each cast
// rejects out-of-range values explicitly.
#![allow(clippy::cast_possible_truncation)]

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;

ubos_events::events! {
    psu_fault() error = "PSU fault",
}

/// PSU status flags decoded from the `TPS6287x` STATUS register.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
// Mirrors a hardware status byte; collapsing the bits into a state
// machine would lose the 1:1 mapping to the register.
#[allow(clippy::struct_excessive_bools)]
pub struct StatusFlags {
    pub current_limit: bool,
    pub thermal_shutdown: bool,
    pub overvoltage: bool,
    pub undervoltage: bool,
}

/// Errors surfaced by the [`Psu`] trait. `E` is the underlying I2C
/// bus error type.
#[derive(Debug)]
pub enum Error<E> {
    Bus(E),
    /// Init failed; only the standardized kind is bubbled up
    /// because there is no `Clone` bound on `E`. The concrete
    /// error is logged at the call site that first observed it.
    InitFailed(embedded_hal_async::i2c::ErrorKind),
    OutOfRange,
    Fault(StatusFlags),
    Terminated,
    /// A bounded retry or poll loop exhausted its budget without ever
    /// observing the expected device state. Distinct from `Bus`: every
    /// individual transaction in the loop completed without a bus
    /// error, but the loop's expected condition never became true.
    Timeout,
}

/// Async PSU control surface. One implementation per hardware
/// platform; consumers stay generic over `Psu`.
pub mod bfm;
pub mod bmm;
pub mod config;
pub mod forge_axe;
pub mod gated;
pub mod ina230;
mod pmbus;
pub mod sic45x;
pub mod state;
pub mod tps53647;
pub mod tps6287x;
pub mod watchdog;

pub use bfm::{BfmDevice, BfmPsu, BfmPsuDescriptor, BfmShared};
pub use bmm::{BmmDevice, BmmPsu, BmmShared};
pub use config::BmmPsuDescriptor;
pub use forge_axe::{ForgeAxeDevice, ForgeAxePsu, ForgeAxePsuDescriptor, ForgeAxeShared};
pub use gated::{GatedPsu, InnerGate};
pub use state::{Device, RawMutex, Ready, Shared, State};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use portable_atomic::{AtomicU32, Ordering as AtomicOrdering};
use ubos_core::units::{Power, Voltage};
use ubos_metrics::metrics::{Metric, Value};

type CSMutex = CriticalSectionRawMutex;

/// A single PSU metrics sample.
///
/// Published by [`run_power_stats`] on every tick. Successful ticks
/// carry fresh values with `sensor_fault = false`; failed ticks carry
/// the most recent known-good values plus `sensor_fault = true` so
/// consumers see a stable last-good reading alongside a fault flag.
#[derive(Clone, Copy)]
pub struct PsuReading {
    /// Input-side power (DC-DC primary).
    pub input_power: Power,
    /// Output-side power (hashboard rail).
    pub output_power: Power,
    /// Voltage last commanded via [`Psu::set_voltage`]. Zero before
    /// the first successful command.
    pub output_voltage_set: Voltage,
    /// Voltage observed on the output rail. Comparing against
    /// `output_voltage_set` exposes droop or sense-line loss.
    pub output_voltage_measured: Voltage,
    /// Monotonic timestamp of this reading in milliseconds since boot.
    pub timestamp_ms: u64,
    /// True when one or more reads on this tick failed and the values
    /// above are the last successful sample carried forward.
    pub sensor_fault: bool,
}

/// Subscriber capacity for the PSU watch. No consumer subscribes
/// today; the REST API peeks via [`Handle::current`], which does not
/// consume a slot. The capacity reserves headroom for future
/// `subscribe()` callers.
const WATCH_CAPACITY: usize = 4;

/// Module-internal state watch. Single writer is [`run_power_stats`];
/// readers obtained via [`Handle::subscribe`] or [`Handle::current`].
pub(crate) static STATE_WATCH: Watch<CSMutex, PsuReading, WATCH_CAPACITY> = Watch::new();

/// Most recent voltage commanded via [`Psu::set_voltage`], in
/// millivolts. Zero means no `set_voltage` call has succeeded yet on
/// this boot. Written by the PSU impl, read by [`run_power_stats`].
static SET_VOLTAGE_MV: AtomicU32 = AtomicU32::new(0);

/// Gauge carrying the last voltage commanded, published from the two
/// places below.
const SET_VOLTAGE_METRIC: &str = "board_psu_output_voltage_set_mv";

/// Record the voltage a successful [`Psu::set_voltage`] set the rail to,
/// so the metrics publisher can include it in the next [`PsuReading`].
///
/// The task that commands the rail calls this with the voltage the
/// driver answered, so [`run_power_stats`] reports the set voltage
/// alongside the measured one.
pub fn record_set_voltage(voltage: Voltage) {
    SET_VOLTAGE_MV.store(voltage.mv(), AtomicOrdering::Relaxed);
}

/// Record a successful `set_voltage` write and publish its gauge, for a
/// driver whose board runs no [`run_power_stats`].
///
/// A supply with nothing to measure has no reason to be polled, but the
/// voltage it was commanded to is still worth exposing, and that gauge
/// otherwise rides the poll loop. One push per setpoint suffices: the
/// metric map holds the last value pushed until it is overwritten, and
/// [`run_power_stats`] republishes every tick only because it is already
/// awake.
pub async fn publish_set_voltage(voltage: Voltage) {
    record_set_voltage(voltage);
    Metric::with_no_labels(SET_VOLTAGE_METRIC, Value::Uint(u64::from(voltage.mv())))
        .push()
        .await;
}

/// Per-task subscription returned by [`Handle::subscribe`].
pub type PsuSubscription = Receiver<'static, CSMutex, PsuReading, WATCH_CAPACITY>;

/// Capacity of the detected PSU model string, matching the system-info
/// API's hardware-identity string width.
pub const MODEL_LEN: usize = 40;

/// The PSU model string a driver identifies during its one-shot init.
pub type ModelString = heapless::String<MODEL_LEN>;

/// Module-internal model watch. Written at most once, by
/// [`run_power_stats`] once the driver identified a model; read via
/// [`Handle::model`].
static MODEL_WATCH: Watch<CSMutex, ModelString, 1> = Watch::new();

/// Obtain a handle to the PSU metrics module. `Copy`; callers freely
/// pass it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
        model: &MODEL_WATCH,
    }
}

/// Consumer-side handle for the PSU metrics.
///
/// `Copy` because it holds only `&'static` references. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, PsuReading, WATCH_CAPACITY>,
    model: &'static Watch<CSMutex, ModelString, 1>,
}

impl Handle {
    /// Last successful voltage command, available before the next metrics poll.
    /// NOTE: This setpoint does not indicate whether the rail is enabled.
    #[must_use]
    pub fn commanded_voltage(&self) -> Option<Voltage> {
        match SET_VOLTAGE_MV.load(AtomicOrdering::Relaxed) {
            0 => None,
            mv => Some(Voltage::from_mv(mv)),
        }
    }

    /// Most recent published reading, or `None` if the PSU task has not
    /// completed a first successful poll yet.
    #[must_use]
    pub fn current(&self) -> Option<PsuReading> {
        self.state.try_get()
    }

    /// Per-task subscription. Call once per consumer task before
    /// entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> PsuSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Detected PSU model, or `None` before the driver identified one.
    #[must_use]
    pub fn model(&self) -> Option<ModelString> {
        self.model.try_get()
    }
}

/// Periodic publisher for INA230 power and voltage readings. Runs forever.
///
/// Reads input power, output power, and the output-rail bus voltage at
/// every [`POWER_STATS_INTERVAL`] tick. Successful reads push to
/// prometheus and publish a fresh [`PsuReading`]; failures log an error
/// and republish the last known values with `sensor_fault = true` so
/// consumers see a stable last-good sample plus a fault flag. Mirrors
/// the publish pattern in `ubos_thermal::run_thermal`. Boards spawn it
/// through [`power_stats_task`], because a task cannot be generic.
///
/// `psu` is a generic parameter rather than a receiver, so no inherent
/// `impl` here can take it. A default method on the [`Psu`] trait could,
/// and would put a publishing loop on the trait every implementor has to
/// carry, so this stays a free function the macro wraps.
// The function reads in a flat sequence (power, voltage, status) and
// pushes metrics inline; splitting into helpers would force the `P:
// Psu` bound and the publisher captures through each one.
#[allow(clippy::too_many_lines)]
pub async fn run_power_stats<P>(psu: &P) -> !
where
    P: Psu,
{
    let publisher = STATE_WATCH.sender();
    let mut ticker = embassy_time::Ticker::every(POWER_STATS_INTERVAL);
    let mut last_known: Option<(Power, Power, Voltage)> = None;
    // Tracks whether a fault was active on the previous poll so the event
    // fires only on the false-to-true transition rather than every tick.
    let mut psu_fault_active = false;
    loop {
        ticker.next().await;
        let input_result = psu.read_input_power().await;
        let output_result = psu.read_output_power().await;
        let voltage_result = psu.read_output_voltage().await;
        let set_voltage = Voltage::from_mv(SET_VOLTAGE_MV.load(AtomicOrdering::Relaxed));
        let now_ms = embassy_time::Instant::now().as_millis();

        match (&input_result, &output_result) {
            (Ok(input), Ok(output)) => {
                ubos_logging::info!(
                    "psu sensors: input={} mW output={} mW",
                    input.mw(),
                    output.mw()
                );
                Metric::with_no_labels(
                    "board_psu_input_power_mw",
                    Value::Uint(u64::from(input.mw())),
                )
                .push()
                .await;
                Metric::with_no_labels(
                    "board_psu_output_power_mw",
                    Value::Uint(u64::from(output.mw())),
                )
                .push()
                .await;
            }
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => {
                ubos_logging::error!(
                    "psu sensor read failed: {:?}",
                    ubos_logging::Debug2Format(error),
                );
            }
            (Err(input_error), Err(output_error)) => {
                ubos_logging::error!(
                    "psu sensor read failed: input={:?} output={:?}",
                    ubos_logging::Debug2Format(input_error),
                    ubos_logging::Debug2Format(output_error),
                );
            }
        }

        match &voltage_result {
            Ok(voltage) => publish_measured_voltage(*voltage, psu.voltage_source()).await,
            Err(error) => {
                ubos_logging::error!(
                    "psu voltage read failed: {:?}",
                    ubos_logging::Debug2Format(error),
                );
            }
        }
        if let Some(secondary) = psu.read_secondary_voltage().await {
            match secondary {
                Ok(voltage) => {
                    publish_measured_voltage(voltage, psu.secondary_voltage_source()).await;
                }
                Err(error) => {
                    ubos_logging::error!(
                        "psu secondary voltage read failed: {:?}",
                        ubos_logging::Debug2Format(&error),
                    );
                }
            }
        }
        if set_voltage.mv() != 0 {
            Metric::with_no_labels(SET_VOLTAGE_METRIC, Value::Uint(u64::from(set_voltage.mv())))
                .push()
                .await;
        }

        match (input_result, output_result, voltage_result) {
            (Ok(input), Ok(output), Ok(voltage)) => {
                last_known = Some((input, output, voltage));
                publisher.send(PsuReading {
                    input_power: input,
                    output_power: output,
                    output_voltage_set: set_voltage,
                    output_voltage_measured: voltage,
                    timestamp_ms: now_ms,
                    sensor_fault: false,
                });
            }
            _ => {
                if let Some((input, output, voltage)) = last_known {
                    publisher.send(PsuReading {
                        input_power: input,
                        output_power: output,
                        output_voltage_set: set_voltage,
                        output_voltage_measured: voltage,
                        timestamp_ms: now_ms,
                        sensor_fault: true,
                    });
                }
            }
        }

        match psu.read_status().await {
            Ok(flags) => {
                Metric::with_no_labels(
                    "board_psu_status_current_limit",
                    Value::Uint(u64::from(flags.current_limit)),
                )
                .push()
                .await;
                Metric::with_no_labels(
                    "board_psu_status_thermal_shutdown",
                    Value::Uint(u64::from(flags.thermal_shutdown)),
                )
                .push()
                .await;
                Metric::with_no_labels(
                    "board_psu_status_overvoltage",
                    Value::Uint(u64::from(flags.overvoltage)),
                )
                .push()
                .await;
                Metric::with_no_labels(
                    "board_psu_status_undervoltage",
                    Value::Uint(u64::from(flags.undervoltage)),
                )
                .push()
                .await;
                let fault_now = flags.current_limit
                    || flags.thermal_shutdown
                    || flags.overvoltage
                    || flags.undervoltage;
                // Emit the fault event and log it once on the rising edge: the
                // SIC latches the rail off in hardware, so a tripped rail must
                // not be silent, but logging every tick would spam.
                if fault_now && !psu_fault_active {
                    crate::events::psu_fault();
                    ubos_logging::warn!(
                        "psu FAULT: overvoltage={} current_limit={} undervoltage={} thermal_shutdown={}",
                        flags.overvoltage,
                        flags.current_limit,
                        flags.undervoltage,
                        flags.thermal_shutdown,
                    );
                }
                psu_fault_active = fault_now;
            }
            // A terminated PSU will not recover; stop polling it so the
            // task does not spam warn logs on every tick indefinitely.
            // Parking here is symmetric with the `(Err(_), Err(_))` power-read
            // path: both represent an unrecoverable device condition and
            // neither can reset the hardware — the miner task owns that.
            Err(Error::Terminated) => {
                ubos_logging::warn!("psu status: device terminated; halting poll loop");
                core::future::pending::<()>().await;
                unreachable!()
            }
            Err(error) => {
                ubos_logging::warn!(
                    "psu status read failed: {:?}",
                    ubos_logging::Debug2Format(&error),
                );
            }
        }
        if MODEL_WATCH.try_get().is_none()
            && let Some(model) = psu.model().await
        {
            MODEL_WATCH.sender().send(model);
        }
    }
}

/// Cadence at which [`run_power_stats`] reads PSU input/output power.
/// Matches the host-runner cadence so the prometheus series rates are
/// comparable across firmware targets.
pub const POWER_STATS_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(2);

/// Publish a measured output-rail voltage, optionally tagged with a `source`
/// label so a board that reads the rail from more than one sensor (e.g. the
/// SIC's `READ_VOUT` and a separate INA230) keeps them as distinct series
/// under one metric name.
async fn publish_measured_voltage(voltage: Voltage, source: Option<&'static str>) {
    use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value};
    let value = Value::Uint(u64::from(voltage.mv()));
    let Some(source) = source else {
        Metric::with_no_labels("board_psu_output_voltage_measured_mv", value)
            .push()
            .await;
        return;
    };
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "source",
        value: LabelValue::Static(source),
    });
    Metric::with_labels("board_psu_output_voltage_measured_mv", labels, value)
        .push()
        .await;
}

pub trait Psu {
    type BusError: embedded_hal_async::i2c::Error;

    /// Enable the output rail, initializing the hardware if needed.
    /// An implementation may wait for startup, but success does not guarantee
    /// that the measured voltage has settled.
    async fn enable(&self) -> Result<(), Error<Self::BusError>>;
    async fn disable(&self) -> Result<(), Error<Self::BusError>>;
    /// Program the rail to `voltage`, or to the nearest value the supply
    /// can produce, and return the voltage the rail was set to.
    async fn set_voltage(&self, voltage: Voltage) -> Result<Voltage, Error<Self::BusError>>;
    async fn read_input_power(&self) -> Result<Power, Error<Self::BusError>>;
    async fn read_output_power(&self) -> Result<Power, Error<Self::BusError>>;
    /// Read the actual output rail voltage. Used to compare the commanded
    /// `set_voltage` against the voltage the hashboard actually sees so
    /// droop or sense-line loss is visible to operators. The source
    /// register is implementation-defined (e.g. `SIC READ_VOUT` for
    /// `BfmPsu`).
    async fn read_output_voltage(&self) -> Result<Voltage, Error<Self::BusError>>;
    /// Optional second sensor measuring the same output rail, for
    /// cross-checking [`read_output_voltage`]. `None` (the default) means the
    /// board exposes only one source.
    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<Self::BusError>>> {
        None
    }
    /// Source label for [`read_output_voltage`], or `None` to publish the
    /// measured-voltage gauge without a `source` label.
    fn voltage_source(&self) -> Option<&'static str> {
        None
    }
    /// Source label for [`read_secondary_voltage`].
    fn secondary_voltage_source(&self) -> Option<&'static str> {
        None
    }
    /// Read the PSU STATUS register and decode the four fault flags.
    async fn read_status(&self) -> Result<StatusFlags, Error<Self::BusError>>;
    /// The model string the driver identified, or `None` before it has
    /// and for a supply that reports none.
    async fn model(&self) -> Option<ModelString> {
        None
    }
    async fn terminate(&self);
}

/// Task shell around [`run_power_stats`]. Takes the board's PSU type.
///
/// Every board that reads a PSU spawns this. A board on a [`BmmPsu`]
/// gets it from [`bmm_psu`]; a board whose power stage is its own
/// wrapper invokes it directly, or from a macro of its own that plays
/// the same role for that wrapper.
#[macro_export]
macro_rules! power_stats_task {
    ($psu:ty) => {
        #[embassy_executor::task]
        pub async fn power_stats_task(psu: &'static $psu) -> ! {
            ubos_psu::run_power_stats(psu).await
        }
    };
}
