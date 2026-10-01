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

//! The PSU task and the handle every other task reaches it through.
//!
//! [`PowerSupply`] and [`Handle`] share this module because the task is
//! the only consumer of the handle's commands and the only writer of the
//! state the handle reads, so that state stays private to the pair.

use core::pin::pin;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use portable_atomic::{AtomicU32, Ordering};
use ubos_core::take_once::TakeOnce;
use ubos_core::units::{Power, Voltage};
use ubos_metrics::catalog::{PSU_POWER, TYPE_LABEL};
use ubos_metrics::metric;
use ubos_metrics::value::Flag;

use crate::{CommandError, Error, ModelString, Psu, VoltageSource};

#[cfg(test)]
#[path = "tests/supply.rs"]
mod tests;

type CSMutex = CriticalSectionRawMutex;

/// One PSU sample, published on every poll. A failed poll republishes the
/// last good values with `sensor_fault` set, so consumers see a stable
/// reading beside the fault.
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
    /// Milliseconds since boot.
    pub timestamp_ms: u64,
    /// True when one or more reads on this poll failed and the values
    /// above are the last successful sample carried forward.
    pub sensor_fault: bool,
}

/// Subscriber capacity for the reading watch. [`Handle::current`] peeks
/// without taking a slot.
const WATCH_CAPACITY: usize = 4;

/// Per-task subscription returned by [`Handle::subscribe`].
pub type PsuSubscription = Receiver<'static, CSMutex, PsuReading, WATCH_CAPACITY>;

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

/// The state behind the [`Handle`]: the commands and replies that pass
/// between the callers and the PSU task, and every fact that task records.
struct Link {
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

static STATE: Link = Link::new();

static TAKEN: TakeOnce = TakeOnce::new("psu");

/// The one way into a running PSU: commands go to [`PowerSupply::run`],
/// and every fact that task records comes back out.
///
/// A firmware takes it once with [`Handle::take`] and passes a copy to
/// each task that reaches the PSU.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the task of the PSU.
///
/// [`PowerSupply::run`] borrows it, so a firmware that gives it to one
/// task cannot give it to a second.
pub struct Runner(());

impl Link {
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
}

impl Handle {
    /// The firmware's one PSU handle and the runner of its task. A second
    /// call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
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
        let _serial = STATE.caller.lock().await;
        let seq = STATE.awaited.load(Ordering::SeqCst);
        STATE.reply.reset();
        // Declared after the caller lock, so the awaited number moves on
        // while the lock is still held and the next caller reads its own.
        let _turn = Turn {
            awaited: &STATE.awaited,
            seq,
        };
        STATE.commands.send((seq, command)).await;
        loop {
            let (reply_seq, reply) = STATE.reply.wait().await;
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
        STATE.setpoint.try_get().map(|setpoint| setpoint.requested)
    }

    /// Most recent published reading, or `None` if the PSU task has not
    /// completed a first successful poll yet.
    #[must_use]
    pub fn current(&self) -> Option<PsuReading> {
        STATE.reading.try_get()
    }

    /// Per-task subscription to the readings. Call once per consumer task
    /// before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> PsuSubscription {
        STATE
            .reading
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Detected PSU model, or `None` before the driver identified one.
    #[must_use]
    pub fn model(&self) -> Option<ModelString> {
        STATE.model.try_get()
    }
}

/// A board's PSU: the driver that the PSU task calls for the commands of
/// the [`Handle`]. The set voltage and the model are recorded where the
/// driver answers rather than by each caller.
pub struct PowerSupply<P: 'static> {
    driver: &'static P,
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
        Self { driver }
    }

    /// Run the PSU forever: carry out each command from the [`Handle`],
    /// poll every [`POWER_STATS_INTERVAL`], and send the heartbeat every
    /// [`Psu::heartbeat_interval`].
    ///
    /// A poll stops between its calls for a queued command and runs again
    /// once that command is answered. A call still running
    /// [`COMMAND_GRACE`] after a command queued behind it is dropped, so a
    /// rail cut waits that long at most. A heartbeat has
    /// [`HEARTBEAT_TIMEOUT`] instead, because a pulse cut short is a missed
    /// one.
    pub async fn run(&self, _runner: &mut Runner) -> ! {
        let mut ticker = embassy_time::Ticker::every(POWER_STATS_INTERVAL);
        let mut heartbeat = self
            .driver
            .heartbeat_interval()
            .await
            .map(embassy_time::Ticker::every);
        let mut poll = Poll {
            last_known: None,
            fault_active: false,
            live: true,
            model_settled: false,
        };
        // Set while a poll that stopped for a command has still to run.
        let mut poll_due = false;
        loop {
            let beat = async {
                match heartbeat.as_mut() {
                    Some(heartbeat) => {
                        heartbeat.next().await;
                        // As for the poll, one pulse follows a held one.
                        heartbeat.reset();
                    }
                    None => core::future::pending().await,
                }
            };
            let poll_tick = async {
                if !poll_due {
                    ticker.next().await;
                }
            };
            // The select takes the first ready arm in this order, so a
            // poll that is due again runs behind the queued command and
            // behind a due heartbeat, and a run of commands that keeps
            // stopping the poll leaves the heartbeat on time.
            match select3(STATE.commands.receive(), beat, poll_tick).await {
                Either3::First(command) => self.answer(command).await,
                Either3::Third(()) => {
                    // The next poll is due one interval from this one, so
                    // a poll held past several intervals is followed by
                    // one and not by one for each interval it missed.
                    ticker.reset();
                    poll_due = poll.live && self.poll(&mut poll).await.is_none();
                }
                Either3::Second(()) => {
                    let pulse = self.driver.heartbeat();
                    match embassy_time::with_timeout(HEARTBEAT_TIMEOUT, pulse).await {
                        Ok(Ok(())) => {}
                        // A terminated supply takes no more heartbeats.
                        Ok(Err(Error::Terminated)) => heartbeat = None,
                        Ok(Err(error)) => {
                            ubos_logging::warn!(
                                "psu heartbeat failed: {:?}",
                                ubos_logging::Debug2Format(&error),
                            );
                        }
                        Err(embassy_time::TimeoutError) => {
                            ubos_logging::warn!("psu heartbeat timed out");
                        }
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
        if seq != STATE.awaited.load(Ordering::SeqCst) {
            return;
        }
        let Some(reply) = self.finish(self.execute(command)).await else {
            // A call dropped as stuck may have reached the supply, so the
            // rail is no longer known to be where the last command set it.
            STATE.setpoint.sender().clear();
            return;
        };
        STATE.reply.signal((seq, reply));
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
                STATE.setpoint.sender().send(Setpoint { requested, landed });
                Ok(())
            }
        }
    }

    /// Run one driver call of a poll, or answer `None` and leave the call
    /// unstarted when a command is queued.
    async fn step<T>(&self, call: impl Future<Output = T>) -> Option<T> {
        if !STATE.commands.is_empty() {
            return None;
        }
        self.finish(call).await
    }

    /// Run a driver call to its end. Once a command queues behind it the
    /// call has [`COMMAND_GRACE`] left, and `None` answers for a call
    /// dropped at the end of it.
    async fn finish<T>(&self, call: impl Future<Output = T>) -> Option<T> {
        let mut call = pin!(call);
        match select(call.as_mut(), STATE.commands.ready_to_receive()).await {
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
                STATE.model.sender().send(model);
            }
        }
        let status_result = self.step(self.driver.read_status()).await?;
        let setpoint = STATE.setpoint.try_get();
        let set_voltage = setpoint.map_or(Voltage::from_mv(0), |setpoint| setpoint.landed);
        let now_ms = embassy_time::Instant::now().as_millis();

        match (&input_result, &output_result) {
            (Ok(input), Ok(output)) => {
                ubos_logging::info!(
                    "psu sensors: input={} mW output={} mW",
                    input.mw(),
                    output.mw()
                );
                metric!(PSU_POWER, *input)
                    .label(TYPE_LABEL, "input")
                    .try_push();
                metric!(PSU_POWER, *output)
                    .label(TYPE_LABEL, "output")
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
                    publish_measured_voltage(voltage, VoltageSource::Feedback);
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
            // The voltage the rail was last set to, pushed on every poll
            // once a command has set it.
            metric!("psu_set_voltage_volts", set_voltage).try_push();
        }

        match (input_result, output_result, voltage_result) {
            (Ok(input), Ok(output), Ok(voltage)) => {
                poll.last_known = Some((input, output, voltage));
                STATE.reading.sender().send(PsuReading {
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
                    STATE.reading.sender().send(PsuReading {
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
                for (flag, raised) in [
                    ("current_limit", flags.current_limit),
                    ("thermal_shutdown", flags.thermal_shutdown),
                    ("overvoltage", flags.overvoltage),
                    ("undervoltage", flags.undervoltage),
                ] {
                    // One series for each status flag of the supply, 1 while
                    // the flag is raised.
                    metric!("psu_status", Flag(raised))
                        .label("flag", flag)
                        .try_push();
                }
                let fault_now = flags.current_limit
                    || flags.thermal_shutdown
                    || flags.overvoltage
                    || flags.undervoltage;
                // Once, on the rising edge: a supply can latch its rail off
                // in hardware, so a trip must not be silent, but every poll
                // would spam.
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

/// Poll cadence. Matches the host runner, so the Prometheus series rates
/// are comparable across targets.
pub const POWER_STATS_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(2);

/// How long a driver call may still run once a command has queued behind
/// it. One bus transfer ends well inside it, so only a stuck call or a
/// long step such as a device init is dropped.
pub const COMMAND_GRACE: embassy_time::Duration = embassy_time::Duration::from_millis(100);

/// How long one heartbeat may run. A healthy pulse ends well inside it,
/// so only a pulse stuck on the bus is dropped.
pub const HEARTBEAT_TIMEOUT: embassy_time::Duration = embassy_time::Duration::from_secs(1);

/// Push a measured voltage of the output rail under the `type` of its
/// source, so a board that reads the rail from the supply and from a
/// separate monitor keeps them as distinct series under one name.
fn publish_measured_voltage(voltage: Voltage, source: VoltageSource) {
    metric!("psu_voltage_volts", voltage)
        .label(TYPE_LABEL, source.label())
        .try_push();
}

/// Task shell around [`PowerSupply::run`]. Takes the board's driver
/// type, because a task cannot be generic.
#[macro_export]
macro_rules! psu_task {
    ($psu:ty) => {
        #[embassy_executor::task]
        pub async fn psu_task(
            psu: &'static ubos_psu::PowerSupply<$psu>,
            mut runner: ubos_psu::Runner,
        ) -> ! {
            psu.run(&mut runner).await
        }
    };
}
