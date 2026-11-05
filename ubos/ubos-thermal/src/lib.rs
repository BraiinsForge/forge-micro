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

//! Thermal sensor task: polls the board temperature sensor and publishes
//! `ThermalReading` via `Handle`. The manager is the sole caller of
//! `MiningHandle::halt`/`resume`, so this task reports facts and never
//! halts anything itself. It sends two independent pairs:
//! `DangerousTemperature`/`TemperatureNormal` on threshold edges, and
//! `TemperatureUnreadable`/`TemperatureReadable` when a sensor that owes
//! an answer stops giving one. The operator's `TemperatureAcknowledged`
//! is the API's to send; this task reads the halt back from the
//! manager's mask on every tick, so a release from either sender is one
//! it sees.
//!
//! The two pairs are separate because they recover differently, which
//! `docs/MINING.md` covers under "Two ways out of an overheat halt".
//!
//! [`Reading::Idle`] is what keeps the second pair honest: why silence
//! on an unpowered board is no fault is under "Chain presence" in
//! `docs/MINING.md`.
//!
//! This task is also the sole writer of the ambient gauge, in `metrics`,
//! and of the MCU temperature gauge, which it reads from the board's
//! [`McuTemperatureSensor`] on each tick.
//! `array` aggregates a board's I2C sensor table into the reading the
//! task polls, over the drivers and the descriptor types in
//! `ubos-temperature`.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

#[cfg(test)]
extern crate std;

pub mod chain;
pub use chain::ChainTemperature;
mod array;
mod metrics;
mod stats;
mod threshold;

pub use array::SensorArray;
pub use stats::{Accumulator, TempStats};

#[cfg(test)]
#[path = "tests/threshold.rs"]
mod tests_threshold;

#[cfg(test)]
#[path = "tests/run_thermal.rs"]
mod tests_run_thermal;

#[cfg(test)]
#[path = "tests/judge.rs"]
mod tests_judge;

/// Serialises every test in this crate that reads the mock clock. They
/// share every static `run_thermal` touches: the clock, the config
/// watch, and `ubos-metrics`'s process-global gauge channel, which the
/// loop pushes the chip temperature into on every measurement. Two of
/// them running concurrently would see each other's gauge pushes and
/// clock resets, and a chain test measuring silence against
/// `Instant::now` would see the clock jump under it.
#[cfg(test)]
static LOOP_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`LOOP_MUTEX`] for one test. A test that panicked under it
/// poisons it, and the next test still needs the clock, so the poison
/// is ignored.
#[cfg(test)]
fn clock() -> std::sync::MutexGuard<'static, ()> {
    LOOP_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The sample a poll produced, or a panic naming what it gave instead.
#[cfg(test)]
fn measured(reading: Reading) -> TempSample {
    match reading {
        Reading::Measured(temps) => temps,
        Reading::Unreadable => panic!("expected a measurement, got Unreadable"),
        Reading::Idle => panic!("expected a measurement, got Idle"),
    }
}

use crate::metrics::{AMBIENT_METRIC_NAME, MCU_METRIC_NAME};
use crate::threshold::{Action, evaluate};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::Handle as ConfigHandle;
use ubos_config::ThermalConfig;
use ubos_core::units::Temperature;
use ubos_logging::{error, info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage, PauseReason, PauseReasons};
use ubos_metrics::metrics::{Metric, Value};

ubos_events::events! {
    critical_temperature(temp_c: i16)   error = "Critical temperature {} °C, mining halted",
    temperature_recovered(temp_c: i16)  info  = "Temperature recovered to {} °C, resuming",
    sensor_fault(misses: u32)           error = "Temperature sensor fault after {} misses",
    sensor_recovered(misses: u32)       info  = "Temperature sensor recovered after {} misses",
}

/// Converts a [`Temperature`] to whole degrees Celsius as [`i16`].
///
/// Mining temperatures are well within the i16 range (-32768..=32767 °C),
/// so the truncating cast from i32 is safe at any physically meaningful value.
#[allow(clippy::cast_possible_truncation)]
fn temp_celsius_i16(temperature: Temperature) -> i16 {
    (temperature.millicelsius() / 1000) as i16
}

type CSMutex = CriticalSectionRawMutex;

/// What a reading measures. `metrics` turns it into the `source` label.
pub use ubos_config::Source;

/// Which reduction of one kind's readings the cooling PID follows.
pub use ubos_config::Statistic;

/// One sample from a board temperature sensor: the hottest and the mean
/// of each kind of reading it took, and `None` for a kind it took none of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempSample {
    /// The dies of the chips.
    pub chip: Option<TempStats>,
    /// The sensors on the hashboard.
    pub hashboard: Option<TempStats>,
}

impl TempSample {
    /// The stats of `source`, when the sample read it.
    #[must_use]
    pub const fn temp_stats(self, source: Source) -> Option<TempStats> {
        match source {
            Source::Chip => self.chip,
            Source::Hashboard => self.hashboard,
        }
    }

    /// Judge each kind this sample read against its own threshold, and
    /// answer the edge of the overheat pair it crossed, if any.
    ///
    /// The counterpart of [`UnreadableHalt`], finding both edges of its
    /// message pair for the same reason. It is a method and not a struct
    /// because this halt has no state of its own: the threshold decision is
    /// [`evaluate`]'s, and whether the halt stands is what `held` says the
    /// manager's mask holds. Reading the mask back each tick is what makes
    /// an operator's release real: a flag of its own would leave this loop
    /// halted for ever after, with `evaluate` answering `Ok` at any
    /// temperature.
    fn judge_dangerous(self, config: &ThermalConfig, held: PauseReasons) -> Option<Note> {
        let halted = held.contains(PauseReason::DangerousTemperature);
        let checks = [Source::Chip, Source::Hashboard].map(|source| {
            match (self.temp_stats(source), config.dangerous.temp(source)) {
                (Some(stats), Some(dangerous)) => Some((stats.max, dangerous)),
                _ => None,
            }
        });
        let mut cooled = checks.iter().any(Option::is_some);
        for (temp, dangerous) in checks.into_iter().flatten() {
            match evaluate(temp, halted, dangerous, config.cooldown_temp) {
                Action::EnterHalt => return Some(Note::Dangerous(temp)),
                Action::Ok => cooled = false,
                Action::Resume => {}
            }
        }
        match (self.chip, self.hashboard) {
            (Some(chip), _) => cooled.then_some(Note::Cooled(chip.max)),
            (None, Some(hashboard)) => cooled.then_some(Note::Cooled(hashboard.max)),
            (None, None) => None,
        }
    }
}

/// What one poll of a board temperature sensor found.
///
/// The three answers differ in what they oblige the loop to do, which is
/// why silence is two variants and not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A usable measurement.
    Measured(TempSample),
    /// The sensor owes an answer and did not give one. Repeated for
    /// [`SENSOR_FAULT_TICKS`] ticks, this halts mining.
    Unreadable,
    /// Nothing the sensor watches is powered, so nothing owes a reading.
    /// Why that is no fault: "Chain presence" in `docs/MINING.md`.
    Idle,
}

/// Board temperature sensor. [`sample`](Self::sample) returns everything
/// one tick decides on, so no reading is cached between calls.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Take one sample. Implementations may await on the underlying
    /// hardware.
    async fn sample(&mut self) -> Reading;

    /// Sum of chip core voltages when this sensor also observes the chain.
    /// `None` means the sensor cannot provide a complete chain measurement.
    fn chain_voltage_mv(&self) -> Option<u32> {
        None
    }
}

/// A sensor for the ambient temperature around the board, apart from
/// the chip sensor. Every board hands [`run_thermal`] one and
/// [`thermal_task!`] takes no default, so every board states whether it
/// reads an ambient temperature and cannot compile without saying so; a
/// board without a sensor states it by handing [`NoAmbient`].
///
/// The reading is for the operator and for the metrics. Nothing in this
/// crate decides on it: it gates no halt and drives no fan. The loop
/// reads it on every tick, after every decision the tick makes, for the
/// reason [`read_ambient`] gives. This paragraph is the one statement
/// of what the reading is for; every other mention points here.
#[allow(async_fn_in_trait)]
pub trait AmbientSensor {
    /// The ambient temperature, or `None` when the read failed.
    ///
    /// The wait is the implementor's to bound. The loop awaits this call
    /// unbounded, and a sensor that never answers ends the ticks, and
    /// with them every judgement the chip sensor feeds. A sensor read
    /// over a link to another controller answers `None` after a
    /// timeout of its own rather than waiting on the link.
    async fn ambient(&mut self) -> Option<Temperature>;
}

/// The ambient temperature sensor of a board that has none: the reading stays `None`.
pub struct NoAmbient;

impl AmbientSensor for NoAmbient {
    async fn ambient(&mut self) -> Option<Temperature> {
        None
    }
}

/// The thermometer on the MCU die, which measures the control board.
/// Every board hands [`run_thermal`] one, for the reason
/// [`AmbientSensor`] gives, and a board whose MCU has none hands
/// [`NoMcu`]. The reading feeds a gauge and nothing else: no control
/// loop reads it.
#[allow(async_fn_in_trait)]
pub trait McuTemperatureSensor {
    /// Read the current MCU die temperature, or `None` if the reading
    /// failed. Implementations may await on the underlying hardware
    /// conversion.
    async fn read(&mut self) -> Option<Temperature>;
}

/// The MCU sensor of a board whose MCU has no thermometer: the reading
/// stays `None`, so the gauge is never pushed.
pub struct NoMcu;

impl McuTemperatureSensor for NoMcu {
    async fn read(&mut self) -> Option<Temperature> {
        None
    }
}

/// A single thermal sample from the board sensor.
///
/// Published by `run_thermal` on every tick that measured something, on
/// every idle tick, and on every unreadable tick past the tolerance
/// budget. Consumers read it through `Handle`.
#[derive(Clone)]
pub struct ThermalReading {
    /// This tick's sample, or `None` on a tick that measured nothing,
    /// which the fan holds full on rather than on a value never measured.
    pub temps: Option<TempSample>,
    /// The ambient temperature around the board, where the board measures
    /// it. What it is and is not read for: [`AmbientSensor`].
    pub ambient: Option<Temperature>,
    /// Chip-derived voltage sum, separate from a measured PSU output rail.
    /// It goes out only beside a measurement, so a tick that measured
    /// nothing reports `None` here as in [`temps`](Self::temps),
    /// and the value is never held over.
    pub chain_voltage_mv: Option<u32>,
    /// Monotonic timestamp of this reading in milliseconds since boot.
    pub timestamp_ms: u64,
}

impl ThermalReading {
    /// Whether this tick measured nothing, so [`temps`](Self::temps)
    /// is `None`.
    ///
    /// The mark covers both kinds of silence, because a consumer of
    /// this watch treats them alike: neither one gives a temperature to
    /// act on. Which kind it is decides whether mining stops, and that
    /// is the manager's pause mask to carry, not this one. The PSU
    /// reading's `sensor_fault` is not mirrored here: a supply that
    /// gives no reading has failed, while a board sensor that gives none
    /// may be watching nothing.
    #[must_use]
    pub const fn chip_reading_stale(&self) -> bool {
        self.temps.is_none()
    }

    /// Ambient temperature in whole degrees Celsius, or `None` where the
    /// board measures none or the read failed. Both info handlers in
    /// `ubos-api` serialize it, so the derivation lives here rather than
    /// twice there.
    #[must_use]
    pub fn ambient_celsius(&self) -> Option<i32> {
        self.ambient.map(Temperature::celsius_i32)
    }
}

/// Subscriber capacity for the thermal watch. Four receivers covers
/// cooling, API, display, and manager without enlarging the watch
/// further for today's consumers.
const WATCH_CAPACITY: usize = 4;

/// State watch carrying the published [`ThermalReading`]. Single writer
/// is [`run_thermal`]; readers obtained via [`Handle::subscribe`].
pub type ThermalWatch = Watch<CSMutex, ThermalReading, WATCH_CAPACITY>;

/// The readings the firmware's one sensor task publishes.
pub(crate) static STATE_WATCH: ThermalWatch = Watch::new();

/// Per-task subscription returned by [`Handle::subscribe`].
pub type ThermalSubscription = Receiver<'static, CSMutex, ThermalReading, WATCH_CAPACITY>;

/// Obtain a handle to the thermal module. `Copy`; callers freely pass
/// it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle::new(&STATE_WATCH)
}

/// Consumer-side handle for the thermal subsystem.
///
/// `Copy` because it holds only a `&'static` reference. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static ThermalWatch,
}

impl Handle {
    /// A handle over `state`. Firmware calls [`handle`] instead, which
    /// answers for the watch the board runs on. A host test that drives
    /// a thermal consumer builds a handle over a watch it declared
    /// itself, and then no other test can observe what it publishes.
    #[must_use]
    pub const fn new(state: &'static ThermalWatch) -> Self {
        Self { state }
    }

    /// Most recent published reading, or `None` if the sensor task has
    /// not yet completed its first poll.
    #[must_use]
    pub fn current(&self) -> Option<ThermalReading> {
        self.state.try_get()
    }

    /// Per-task subscription. Call once per consumer task before
    /// entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> ThermalSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }
}

/// Consecutive [`Reading::Unreadable`] answers tolerated before
/// declaring the bus dead. At a 1 Hz sample interval this is 5 seconds,
/// long enough to ride out a transient I2C glitch but short enough to
/// react if the bus has gone permanently silent (stuck SDA, dead chip,
/// broken cable). On fault the loop halts mining and parks the fan at
/// 100% until a reading clears it.
///
/// [`Reading::Idle`] does not count against this budget and resets it. A
/// board with nothing powered is not a board with a broken sensor.
///
/// Exposed `pub` so `ubos-cooling` can assert at compile time that its
/// `STALE_INTERVAL_MULTIPLIER` stays at least `SENSOR_FAULT_TICKS + 2`.
/// Lowering that margin fails the cooling crate's build.
pub const SENSOR_FAULT_TICKS: u32 = 5;

/// Consecutive [`Reading::Unreadable`] answers, and the halt this loop
/// owes the manager once they run past [`SENSOR_FAULT_TICKS`].
///
/// The count is the budget and nothing more. Whether the halt is held
/// is the manager's mask to say, read back the same way the overheat
/// halt is, so no copy of it lives here to fall out of step. The message
/// pair is asymmetric — only the sender of
/// [`ManagerMessage::TemperatureUnreadable`] may retract it, and it
/// must — so both edges are found here, by [`miss`](Self::miss) and
/// [`clear`](Self::clear), rather than one of them at a call site.
#[derive(Default)]
struct UnreadableHalt {
    ticks: u32,
}

impl UnreadableHalt {
    /// Count one unreadable answer. Answers the count once the budget
    /// is spent, and `None` inside it, so the one call says both how
    /// far the silence has run and whether the loop still rides it out.
    fn miss(&mut self) -> Option<u32> {
        self.ticks = self.ticks.saturating_add(1);
        (self.ticks >= SENSOR_FAULT_TICKS).then_some(self.ticks)
    }

    /// Reset the count, and answer the misses a halt held in `held` had
    /// counted, which is the caller's cue to release it. `None` when
    /// there was no halt to release.
    ///
    /// Called both when a reading returns and when the board goes idle,
    /// because either one ends the condition that halted mining. The idle
    /// case is the one that matters, for the reason `docs/MINING.md`
    /// gives under "Chain presence": a halt that waited for a reading to
    /// release it would never release at all.
    fn clear(&mut self, held: PauseReasons) -> Option<u32> {
        let misses = core::mem::take(&mut self.ticks);
        held.contains(PauseReason::TemperatureUnreadable)
            .then_some(misses)
    }
}

/// An edge one tick crossed, which the manager has to hear about. Each
/// variant is one message, and the loop sends it and logs the edge, so
/// the decision that found it stays a plain function.
#[derive(Debug, PartialEq, Eq)]
enum Note {
    /// The miss budget is spent and no halt is held yet. Carries the
    /// misses counted.
    Unreadable(u32),
    /// A reading returned under the unreadable halt. Carries the misses
    /// the halt had counted.
    Recovered(u32),
    /// The board went idle under the unreadable halt.
    Idle,
    /// A reading crossed the dangerous threshold of its kind.
    Dangerous(Temperature),
    /// Every kind read cooled below the cooldown threshold while the
    /// overheat halt was held. Carries the hottest die where one was
    /// read, and the hottest hashboard reading otherwise.
    Cooled(Temperature),
}

impl Note {
    /// Tell the manager, and log the edge.
    async fn report(self, manager_handle: &ManagerHandle) {
        let message = match self {
            Self::Unreadable(ticks) => {
                crate::events::sensor_fault(ticks);
                error!(
                    "thermal: sensor unreadable for {} ticks; halting + forcing fan to 100%",
                    ticks
                );
                ManagerMessage::TemperatureUnreadable
            }
            Self::Recovered(misses) => {
                crate::events::sensor_recovered(misses);
                info!("thermal: sensor recovered after {} misses", misses);
                ManagerMessage::TemperatureReadable
            }
            Self::Idle => {
                info!("thermal: sensor owes no reading, releasing the halt");
                ManagerMessage::TemperatureReadable
            }
            Self::Dangerous(temp) => {
                error!(
                    "thermal: dangerous temperature {}°C, halting mining",
                    temp.celsius_f64()
                );
                crate::events::critical_temperature(temp_celsius_i16(temp));
                ManagerMessage::DangerousTemperature
            }
            Self::Cooled(temp) => {
                warn!(
                    "thermal: cooled to {}°C, resuming mining",
                    temp.celsius_f64()
                );
                crate::events::temperature_recovered(temp_celsius_i16(temp));
                ManagerMessage::TemperatureNormal
            }
        };
        manager_handle.send(message).await;
    }
}

/// What a tick decided from the sensor's answer, for the loop to act on.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    /// Whether a reading goes out, and what it carries.
    tick: Tick,
    /// The edge of the unreadable pair this tick crossed, if any.
    unreadable: Option<Note>,
    /// The edge of the overheat pair this tick crossed, if any. Only a
    /// measurement can cross one, and it can cross both pairs' at once.
    dangerous: Option<Note>,
}

/// Whether a reading goes out this tick, and the temperatures it
/// carries.
#[derive(Debug, PartialEq, Eq)]
enum Tick {
    /// Publish a reading: this tick's sample, `None` on a tick that
    /// measured nothing.
    Publish { temps: Option<TempSample> },
    /// Inside the miss budget: say nothing this tick. Silence that short
    /// is a hiccup the loop rides out, and a reading for it would park
    /// the fan at full for a glitch.
    Withhold,
}

/// What one tick hands the next: the miss budget.
#[derive(Default)]
struct Thermal {
    unreadable_halt: UnreadableHalt,
}

impl Thermal {
    /// Decide one tick from the sensor's answer: count or clear the
    /// miss budget, find the halts and releases the answer calls for
    /// against the halts `held` says the manager holds, and say what to
    /// publish. It performs nothing: the loop reads the mask, polls the
    /// sensor, and acts on the step, so this is a plain function of its
    /// arguments and the previous tick.
    fn judge(&mut self, reading: Reading, config: &ThermalConfig, held: PauseReasons) -> Step {
        match reading {
            Reading::Measured(temps) => Step {
                tick: Tick::Publish { temps: Some(temps) },
                unreadable: self.unreadable_halt.clear(held).map(Note::Recovered),
                dangerous: temps.judge_dangerous(config, held),
            },
            Reading::Unreadable => {
                // Past the budget the board runs unwatched and every
                // consumer hears so on every tick.
                let Some(ticks) = self.unreadable_halt.miss() else {
                    return Step {
                        tick: Tick::Withhold,
                        unreadable: None,
                        dangerous: None,
                    };
                };
                let note = (!held.contains(PauseReason::TemperatureUnreadable))
                    .then_some(Note::Unreadable(ticks));
                Self::held(note)
            }
            Reading::Idle => {
                // Why an idle board releases the halt: "Chain presence" in
                // `docs/MINING.md`. It has nothing to measure, so every
                // consumer hears on the first idle tick that the reading
                // is held.
                let note = self.unreadable_halt.clear(held).map(|_| Note::Idle);
                Self::held(note)
            }
        }
    }

    /// The step of a tick that measured nothing: a reading goes out with
    /// no sample, and `unreadable` is the one edge such a tick can cross.
    fn held(unreadable: Option<Note>) -> Step {
        Step {
            tick: Tick::Publish { temps: None },
            unreadable,
            dangerous: None,
        }
    }
}

/// Read the ambient temperature and publish its gauge. It gates
/// nothing, so it is read after every decision a tick makes and never
/// before one: a sensor that takes its time answering must not hold up
/// the sample the halt turns on, nor the halt itself. Every tick reads
/// it, including one that publishes no reading, because the chip
/// sensor's silence says nothing about this sensor; the watch carries
/// the value of the last tick that published.
async fn read_ambient<A: AmbientSensor>(ambient: &mut A) -> Option<Temperature> {
    let reading = ambient.ambient().await;
    if let Some(reading) = reading {
        let value = Value::Int(i64::from(reading.millicelsius()));
        Metric::with_no_labels(AMBIENT_METRIC_NAME, value)
            .push()
            .await;
    }
    reading
}

/// Run the thermal monitoring loop forever.
///
/// Spawned once per board firmware. `sensor` is the board-specific
/// temperature sensor driver and `ambient` its ambient temperature
/// sensor, [`NoAmbient`] where it has none. `mcu` is the thermometer on
/// the MCU die, [`NoMcu`] where the MCU has none, and the loop pushes its
/// reading to the metrics on each tick. `manager_handle` takes the two
/// message pairs this loop sends, and is also where it reads both halts
/// back from: the manager's mask is the only copy of that state, so an
/// operator who clears a thermal halt cannot leave this loop believing
/// it is still halted and the trip disarmed. A send returns before the
/// manager applies it, so a read-back on the same tick could still show
/// the old mask; the manager's turn is over long before the next tick,
/// and a duplicate message moves no bit, see `ubos_manager::Handle::send`.
///
/// The manager is the sole caller of `MiningHandle::halt`/`resume`.
/// Thermal tuning is read from `config` and refreshed whenever that
/// config publishes a new value. Board firmware passes
/// [`ubos_config::handle`], which [`thermal_task`] supplies for it.
pub async fn run_thermal<S: TemperatureSensor, A: AmbientSensor, M: McuTemperatureSensor>(
    mut sensor: S,
    mut ambient: A,
    mut mcu: M,
    manager_handle: ManagerHandle,
    config_handle: ConfigHandle,
) -> ! {
    let publisher = STATE_WATCH.sender();
    let mut config_subscription = config_handle.subscribe();
    let mut config: ThermalConfig = config_handle.current().thermal;

    let mut sample_interval = Duration::from_millis(u64::from(config.sample_interval_ms));
    let mut ticker = Ticker::every(sample_interval);
    let mut late_threshold = sample_interval * 2;

    let mut thermal = Thermal::default();
    let mut prev_tick = Instant::now();

    info!("thermal: started");

    loop {
        ticker.next().await;
        if let Some(updated) = config_subscription.try_changed() {
            let new_thermal = updated.thermal;
            if new_thermal.sample_interval_ms != config.sample_interval_ms {
                sample_interval = Duration::from_millis(u64::from(new_thermal.sample_interval_ms));
                ticker = Ticker::every(sample_interval);
                late_threshold = sample_interval * 2;
                prev_tick = Instant::now();
                info!(
                    "thermal: sample interval updated to {} ms",
                    new_thermal.sample_interval_ms
                );
            }
            config = new_thermal;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(prev_tick);
        if elapsed > late_threshold {
            warn!("thermal: tick late by {} ms", elapsed.as_millis());
        }
        prev_tick = now;

        let reading = sensor.sample().await;
        let step = thermal.judge(reading, &config, manager_handle.pause_reasons());
        for note in [step.unreadable, step.dangerous].into_iter().flatten() {
            note.report(&manager_handle).await;
        }
        let ambient_temp = read_ambient(&mut ambient).await;
        // A tick inside the miss budget withholds its reading, for the
        // reason `Tick::Withhold` gives.
        if let Tick::Publish { temps } = step.tick {
            // The chain voltage goes out only beside a measurement, and a
            // tick that measured nothing reports none.
            publisher.send(ThermalReading {
                temps,
                ambient: ambient_temp,
                chain_voltage_mv: temps.and_then(|_| sensor.chain_voltage_mv()),
                timestamp_ms: now.as_millis(),
            });
        }
        // Read last, after the reading the cooling loop turns on has gone
        // out. The gauge is dropped on a full metrics channel, because this
        // safety loop must not wait on the metrics, and the next tick
        // replaces it.
        if let Some(temp) = mcu.read().await {
            let value = Value::Int(i64::from(temp.millicelsius()));
            Metric::with_no_labels(MCU_METRIC_NAME, value).try_push();
        }
    }
}

/// Task shell around [`run_thermal`], which polls the temperature
/// sensors and publishes the thermal reading. Takes the board's sensor
/// type, an I2C sensor array on most boards, and its ambient temperature sensor type,
/// [`NoAmbient`] on a board without one, and its MCU thermometer type,
/// [`NoMcu`] on an MCU without one. Neither
/// type has a default, for the reason [`AmbientSensor`] gives.
#[macro_export]
macro_rules! thermal_task {
    ($sensor:ty, $ambient:ty, $mcu:ty) => {
        #[embassy_executor::task]
        async fn thermal_task(
            sensor: $sensor,
            ambient: $ambient,
            mcu: $mcu,
            manager_handle: ubos_manager::Handle,
        ) -> ! {
            ubos_thermal::run_thermal(sensor, ambient, mcu, manager_handle, ubos_config::handle())
                .await
        }
    };
}
