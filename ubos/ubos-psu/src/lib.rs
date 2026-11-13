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
//! A board builds one [`PowerSupply`] over its driver and spawns
//! [`PowerSupply::run`]. That task is the only caller of the driver's
//! rail commands and the only writer of what it records about them;
//! every other task reaches the PSU through the `&'static` [`Handle`]
//! the board passes it at spawn.

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

use core::pin::pin;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embedded_hal_async::i2c::ErrorKind;
use portable_atomic::{AtomicU32, Ordering};
use ubos_core::units::{Power, Voltage};
use ubos_metrics::metrics::{Metric, Value};

type CSMutex = CriticalSectionRawMutex;

/// A single PSU metrics sample.
///
/// Published by [`PowerSupply::run`] on every poll. Successful polls
/// carry fresh values with `sensor_fault = false`; failed polls carry
/// the most recent known-good values plus `sensor_fault = true` so
/// consumers see a stable last-good reading alongside a fault flag.
#[derive(Clone, Copy)]
pub struct PsuReading {
    /// Input-side power (DC-DC primary).
    pub input_power: Power,
    /// Output-side power (hashboard rail).
    pub output_power: Power,
    /// Voltage the rail was last set to. Zero before the first
    /// successful command.
    pub output_voltage_set: Voltage,
    /// Voltage observed on the output rail. Comparing against
    /// `output_voltage_set` exposes droop or sense-line loss.
    pub output_voltage_measured: Voltage,
    /// Monotonic timestamp of this reading in milliseconds since boot.
    pub timestamp_ms: u64,
    /// True when one or more reads on this poll failed and the values
    /// above are the last successful sample carried forward.
    pub sensor_fault: bool,
}

/// Subscriber capacity for the reading watch. No consumer subscribes
/// today; the REST API and the miner peek via [`Handle::current`], which
/// does not consume a slot.
const WATCH_CAPACITY: usize = 4;

/// Gauge carrying the voltage the rail was last set to, published on
/// every poll once a command has set it.
const SET_VOLTAGE_METRIC: &str = "board_psu_output_voltage_set_mv";

/// Per-task subscription returned by [`Handle::subscribe`].
pub type PsuSubscription = Receiver<'static, CSMutex, PsuReading, WATCH_CAPACITY>;

/// Capacity of the detected PSU model string, matching the system-info
/// API's hardware-identity string width.
pub const MODEL_LEN: usize = 40;

/// The PSU model string a driver identifies during its one-shot init.
pub type ModelString = heapless::String<MODEL_LEN>;

/// The error a [`Handle`] command answers with: the driver's error with
/// its bus error reduced to the standard kind, so no consumer is generic
/// over the board's bus.
pub type CommandError = Error<ErrorKind>;

/// A command the PSU task carries out on the driver.
enum Command {
    Enable,
    Disable,
    SetVoltage(Voltage),
}

/// The voltage a successful command asked for and the voltage the supply
/// landed on, which differ by up to one step of the supply.
#[derive(Clone, Copy)]
struct Setpoint {
    requested: Voltage,
    landed: Voltage,
}

/// Ends a caller's turn on the handle when the caller returns or is
/// dropped, by moving the awaited sequence number past its own.
///
/// A caller dropped before the PSU task took its command would otherwise
/// leave an enable or a voltage write queued, and the task would start it
/// ahead of the rail cut that follows a halt.
struct Turn<'a> {
    awaited: &'a AtomicU32,
    seq: u32,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.awaited
            .store(self.seq.wrapping_add(1), Ordering::SeqCst);
    }
}

/// The one way into a running PSU: commands go to [`PowerSupply::run`],
/// and every fact that task records comes back out.
///
/// [`PowerSupply::handle`] hands it out as a `'static` reference, so
/// every task borrows the one instance the supply owns. The task is the
/// only consumer of the commands and the only writer of the state the
/// handle reads.
pub struct Handle {
    commands: Channel<CSMutex, (u32, Command), 1>,
    /// One reply slot, tagged with the sequence number of the command it
    /// answers, see [`Handle::command`].
    reply: Signal<CSMutex, (u32, Result<(), CommandError>)>,
    /// Sequence number of the command a caller awaits, or of the next
    /// command while no caller waits. The PSU task starts a command only
    /// while its number is the awaited one.
    awaited: AtomicU32,
    /// Serialises callers. Two callers at once would share the one queue
    /// slot and the one reply slot, and the second would move the awaited
    /// number while the first still waits on it.
    caller: Mutex<CSMutex, ()>,
    reading: Watch<CSMutex, PsuReading, WATCH_CAPACITY>,
    setpoint: Watch<CSMutex, Setpoint, 1>,
    model: Watch<CSMutex, ModelString, 1>,
}

impl Handle {
    /// The handle a board holds when it has no supply. Every read
    /// answers `None`. A command sent to it waits forever, so such a
    /// board spawns only the tasks that read the PSU.
    #[must_use]
    pub const fn absent() -> Self {
        Self::new()
    }

    const fn new() -> Self {
        Self {
            commands: Channel::new(),
            reply: Signal::new(),
            awaited: AtomicU32::new(0),
            caller: Mutex::new(()),
            reading: Watch::new(),
            setpoint: Watch::new(),
            model: Watch::new(),
        }
    }

    /// Send `command` stamped with the awaited sequence number and await
    /// the reply tagged with that same number.
    ///
    /// The miner races PSU commands against STOP and halt, so a caller can
    /// be dropped between the send and its reply. A command the task has
    /// started by then is still carried out and answered once, unless it
    /// is stuck, and the next caller discards that orphan rather than
    /// taking it as its own.
    /// A command still queued is skipped by the task, so the task starts
    /// only a command somebody awaits.
    async fn command(&self, command: Command) -> Result<(), CommandError> {
        let _serial = self.caller.lock().await;
        let seq = self.awaited.load(Ordering::SeqCst);
        self.reply.reset();
        // Declared after the caller lock, so the awaited number moves on
        // while the lock is still held and the next caller reads its own.
        let _turn = Turn {
            awaited: &self.awaited,
            seq,
        };
        self.commands.send((seq, command)).await;
        loop {
            let (reply_seq, reply) = self.reply.wait().await;
            if reply_seq == seq {
                return reply;
            }
        }
    }

    /// Enable the output rail.
    ///
    /// # Errors
    ///
    /// The driver's error, with its bus error reduced to the kind.
    pub async fn enable(&self) -> Result<(), CommandError> {
        self.command(Command::Enable).await
    }

    /// Disable the output rail.
    ///
    /// # Errors
    ///
    /// The driver's error, with its bus error reduced to the kind.
    pub async fn disable(&self) -> Result<(), CommandError> {
        self.command(Command::Disable).await
    }

    /// Set the rail to `voltage`, or to the nearest value the supply can
    /// produce. [`Handle::commanded_voltage`] answers `voltage` from then
    /// on, and [`PsuReading::output_voltage_set`] carries the value the
    /// supply landed on.
    ///
    /// # Errors
    ///
    /// The driver's error, with its bus error reduced to the kind.
    pub async fn set_voltage(&self, voltage: Voltage) -> Result<(), CommandError> {
        self.command(Command::SetVoltage(voltage)).await
    }

    /// The voltage the last successful [`Handle::set_voltage`] asked for,
    /// or `None` before one. A caller compares its next request with this
    /// value, because the supply can land a request on a nearby step. The
    /// value stays as it is while the rail is disabled, and is `None`
    /// again once a command was dropped as stuck, so the next request is
    /// written whatever it asks for.
    #[must_use]
    pub fn commanded_voltage(&self) -> Option<Voltage> {
        self.setpoint.try_get().map(|setpoint| setpoint.requested)
    }

    /// Most recent published reading, or `None` if the PSU task has not
    /// completed a first successful poll yet.
    #[must_use]
    pub fn current(&self) -> Option<PsuReading> {
        self.reading.try_get()
    }

    /// Per-task subscription to the readings. Call once per consumer task
    /// before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&'static self) -> PsuSubscription {
        self.reading
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Detected PSU model, or `None` before the driver identified one.
    #[must_use]
    pub fn model(&self) -> Option<ModelString> {
        self.model.try_get()
    }
}

/// A board's PSU: the driver and the [`Handle`] every other task reaches
/// it through.
///
/// [`PowerSupply::run`] is the only caller of the driver's rail commands
/// and the only writer of what the handle reads, so the set voltage and
/// the model are recorded where the driver answers rather than by each
/// caller.
pub struct PowerSupply<P: 'static> {
    driver: &'static P,
    handle: Handle,
}

/// What one poll hands the next.
struct Poll {
    /// The last fully successful sample, republished with a fault flag
    /// when a later poll fails.
    last_known: Option<(Power, Power, Voltage)>,
    /// Whether a fault was active on the previous poll, so the event
    /// fires only on the rising edge rather than every poll.
    fault_active: bool,
    /// Cleared once the driver reports itself terminated, which it never
    /// recovers from.
    live: bool,
    /// Set once the driver has named its model, or has answered that it
    /// has none on a poll whose read succeeded, so a supply that reports
    /// no model is asked once.
    model_settled: bool,
}

impl<P: Psu> PowerSupply<P> {
    /// A supply over `driver`. The first call into the driver comes
    /// from [`PowerSupply::run`].
    #[must_use]
    pub const fn new(driver: &'static P) -> Self {
        Self {
            driver,
            handle: Handle::new(),
        }
    }

    /// The handle the board passes to every task that reaches the PSU.
    #[must_use]
    pub fn handle(&'static self) -> &'static Handle {
        &self.handle
    }

    /// Run the PSU forever: carry out each command from the [`Handle`]
    /// and record its outcome, and every [`POWER_STATS_INTERVAL`] read
    /// the power, the voltage and the status and publish them. Boards
    /// spawn it through [`psu_task`].
    ///
    /// A driver call the task started runs on while no command waits
    /// behind it. A poll stops between its calls for a queued command,
    /// and runs again once that command is answered. A call still
    /// running [`COMMAND_GRACE`] after a command queued behind it is
    /// dropped, whether it is stuck on the bus or inside a long step such
    /// as a device init, so a rail cut waits that long at most before it
    /// starts. A driver built on [`state::Shared`] runs a dropped init
    /// again on its next call.
    pub async fn run(&self) -> ! {
        let mut ticker = embassy_time::Ticker::every(POWER_STATS_INTERVAL);
        let mut poll = Poll {
            last_known: None,
            fault_active: false,
            live: true,
            model_settled: false,
        };
        loop {
            match select(self.handle.commands.receive(), ticker.next()).await {
                Either::First(command) => self.answer(command).await,
                Either::Second(()) => {
                    while poll.live && self.poll(&mut poll).await.is_none() {
                        let command = self.handle.commands.receive().await;
                        self.answer(command).await;
                    }
                }
            }
        }
    }

    /// Carry out a command its caller still awaits, and answer it.
    ///
    /// A command whose caller was dropped while it was queued is skipped,
    /// so a rail cut never waits behind a write nobody started. A newer
    /// command queues behind a running one only once the caller of the
    /// running one was dropped, so a command dropped as stuck has nobody
    /// to answer.
    async fn answer(&self, (seq, command): (u32, Command)) {
        if seq != self.handle.awaited.load(Ordering::SeqCst) {
            return;
        }
        let Some(reply) = self.finish(self.execute(command)).await else {
            // A call dropped as stuck may have reached the supply, so the
            // rail is no longer known to be where the last command set it.
            self.handle.setpoint.sender().clear();
            return;
        };
        self.handle.reply.signal((seq, reply));
    }

    /// Carry out one command on the driver and record what it changed.
    async fn execute(&self, command: Command) -> Result<(), CommandError> {
        match command {
            Command::Enable => self.driver.enable().await.map_err(Error::reduce),
            Command::Disable => self.driver.disable().await.map_err(Error::reduce),
            Command::SetVoltage(requested) => {
                let landed = self
                    .driver
                    .set_voltage(requested)
                    .await
                    .map_err(Error::reduce)?;
                self.handle
                    .setpoint
                    .sender()
                    .send(Setpoint { requested, landed });
                Ok(())
            }
        }
    }

    /// Run one driver call of a poll, or answer `None` and leave the call
    /// unstarted when a command is queued.
    async fn step<T>(&self, call: impl Future<Output = T>) -> Option<T> {
        if !self.handle.commands.is_empty() {
            return None;
        }
        self.finish(call).await
    }

    /// Run a driver call to its end. Once a command queues behind it the
    /// call has [`COMMAND_GRACE`] left, and `None` answers for a call
    /// dropped at the end of it.
    async fn finish<T>(&self, call: impl Future<Output = T>) -> Option<T> {
        let mut call = pin!(call);
        match select(call.as_mut(), self.handle.commands.ready_to_receive()).await {
            Either::First(output) => Some(output),
            Either::Second(()) => embassy_time::with_timeout(COMMAND_GRACE, call).await.ok(),
        }
    }

    /// One poll: read the power, the voltage and the status, push their
    /// gauges, and publish a [`PsuReading`]. Answers `None` when it
    /// stopped for a queued command. A gauge is dropped when the metrics
    /// channel is full, so a rail command never waits on the channel's
    /// reader, and the next poll pushes the gauge again. A status flag
    /// the supply clears on read is pushed by the one poll that read it,
    /// and the fault event carries it when that gauge is dropped.
    ///
    /// Every driver call comes before the first gauge, so a poll that
    /// stops for a command has published nothing and the poll that runs
    /// again publishes once. The status read is the last call, because a
    /// supply can clear its status on read, and a poll that stopped behind
    /// it would lose the flags it read.
    // The function reads the power, the voltage and the status in a flat
    // sequence and pushes metrics inline; splitting it would only move the
    // pushes.
    #[allow(clippy::too_many_lines)]
    async fn poll(&self, poll: &mut Poll) -> Option<()> {
        let input_result = self.step(self.driver.read_input_power()).await?;
        let output_result = self.step(self.driver.read_output_power()).await?;
        let voltage_result = self.step(self.driver.read_output_voltage()).await?;
        let secondary_result = self.step(self.driver.read_secondary_voltage()).await?;
        // A read that succeeded ran the driver's init, which is where it
        // identifies its part, so its answer on such a poll is final.
        if !poll.model_settled {
            let initialized =
                input_result.is_ok() || output_result.is_ok() || voltage_result.is_ok();
            let model = self.step(self.driver.model()).await?;
            poll.model_settled = initialized || model.is_some();
            if let Some(model) = model {
                self.handle.model.sender().send(model);
            }
        }
        let status_result = self.step(self.driver.read_status()).await?;
        let setpoint = self.handle.setpoint.try_get();
        let set_voltage = setpoint.map_or(Voltage::from_mv(0), |setpoint| setpoint.landed);
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
                .try_push();
                Metric::with_no_labels(
                    "board_psu_output_power_mw",
                    Value::Uint(u64::from(output.mw())),
                )
                .try_push();
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
            Ok(voltage) => publish_measured_voltage(*voltage, self.driver.voltage_source()),
            Err(error) => {
                ubos_logging::error!(
                    "psu voltage read failed: {:?}",
                    ubos_logging::Debug2Format(error),
                );
            }
        }
        if let Some(secondary) = secondary_result {
            match secondary {
                Ok(voltage) => {
                    publish_measured_voltage(voltage, self.driver.secondary_voltage_source());
                }
                Err(error) => {
                    ubos_logging::error!(
                        "psu secondary voltage read failed: {:?}",
                        ubos_logging::Debug2Format(&error),
                    );
                }
            }
        }
        if setpoint.is_some() {
            Metric::with_no_labels(SET_VOLTAGE_METRIC, Value::Uint(u64::from(set_voltage.mv())))
                .try_push();
        }

        match (input_result, output_result, voltage_result) {
            (Ok(input), Ok(output), Ok(voltage)) => {
                poll.last_known = Some((input, output, voltage));
                self.handle.reading.sender().send(PsuReading {
                    input_power: input,
                    output_power: output,
                    output_voltage_set: set_voltage,
                    output_voltage_measured: voltage,
                    timestamp_ms: now_ms,
                    sensor_fault: false,
                });
            }
            _ => {
                if let Some((input, output, voltage)) = poll.last_known {
                    self.handle.reading.sender().send(PsuReading {
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

        match status_result {
            Ok(flags) => {
                Metric::with_no_labels(
                    "board_psu_status_current_limit",
                    Value::Uint(u64::from(flags.current_limit)),
                )
                .try_push();
                Metric::with_no_labels(
                    "board_psu_status_thermal_shutdown",
                    Value::Uint(u64::from(flags.thermal_shutdown)),
                )
                .try_push();
                Metric::with_no_labels(
                    "board_psu_status_overvoltage",
                    Value::Uint(u64::from(flags.overvoltage)),
                )
                .try_push();
                Metric::with_no_labels(
                    "board_psu_status_undervoltage",
                    Value::Uint(u64::from(flags.undervoltage)),
                )
                .try_push();
                let fault_now = flags.current_limit
                    || flags.thermal_shutdown
                    || flags.overvoltage
                    || flags.undervoltage;
                // Emit the fault event and log it once on the rising edge: the
                // SIC latches the rail off in hardware, so a tripped rail must
                // not be silent, but logging every tick would spam.
                if fault_now && !poll.fault_active {
                    crate::events::psu_fault();
                    ubos_logging::warn!(
                        "psu FAULT: overvoltage={} current_limit={} undervoltage={} thermal_shutdown={}",
                        flags.overvoltage,
                        flags.current_limit,
                        flags.undervoltage,
                        flags.thermal_shutdown,
                    );
                }
                poll.fault_active = fault_now;
            }
            // A terminated PSU stays terminated, so the polling stops and
            // the log stays quiet. The task goes on answering commands,
            // which the driver refuses.
            Err(Error::Terminated) => {
                ubos_logging::warn!("psu status: device terminated; polling stops");
                poll.live = false;
            }
            Err(error) => {
                ubos_logging::warn!(
                    "psu status read failed: {:?}",
                    ubos_logging::Debug2Format(&error),
                );
            }
        }
        Some(())
    }
}

/// Cadence at which [`PowerSupply::run`] reads PSU input/output power.
/// Matches the host-runner cadence so the prometheus series rates are
/// comparable across firmware targets.
pub const POWER_STATS_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(2);

/// How long a driver call may still run once a command has queued behind
/// it. One bus transfer ends well inside it. A call that holds longer,
/// such as a device init or a write with retries, is dropped with a
/// stuck one, so the command behind it starts on time.
pub const COMMAND_GRACE: embassy_time::Duration = embassy_time::Duration::from_millis(100);

/// Publish a measured output-rail voltage, optionally tagged with a `source`
/// label so a board that reads the rail from more than one sensor (e.g. the
/// SIC's `READ_VOUT` and a separate INA230) keeps them as distinct series
/// under one metric name.
fn publish_measured_voltage(voltage: Voltage, source: Option<&'static str>) {
    use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value};
    let value = Value::Uint(u64::from(voltage.mv()));
    let Some(source) = source else {
        Metric::with_no_labels("board_psu_output_voltage_measured_mv", value).try_push();
        return;
    };
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "source",
        value: LabelValue::Static(source),
    });
    Metric::with_labels("board_psu_output_voltage_measured_mv", labels, value).try_push();
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

/// Task shell around [`PowerSupply::run`]. Takes the board's driver
/// type, because a task cannot be generic.
#[macro_export]
macro_rules! psu_task {
    ($psu:ty) => {
        #[embassy_executor::task]
        pub async fn psu_task(psu: &'static ubos_psu::PowerSupply<$psu>) -> ! {
            psu.run().await
        }
    };
}

impl<E: embedded_hal_async::i2c::Error> Error<E> {
    /// Reduce a driver error to the standard bus error kind.
    fn reduce(self) -> CommandError {
        match self {
            Error::Bus(bus) => Error::Bus(bus.kind()),
            Error::InitFailed(kind) => Error::InitFailed(kind),
            Error::OutOfRange => Error::OutOfRange,
            Error::Fault(flags) => Error::Fault(flags),
            Error::Terminated => Error::Terminated,
            Error::Timeout => Error::Timeout,
        }
    }
}
