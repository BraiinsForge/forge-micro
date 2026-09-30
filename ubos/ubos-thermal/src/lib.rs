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
//! This task is also the sole writer of the chip temperature gauge and
//! the ambient gauge, in `metrics`. `array` aggregates a board's I2C
//! sensor table into the reading the task polls, over the drivers and
//! the descriptor types in `ubos-temperature`.

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
mod threshold;

pub use array::SensorArray;

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
fn measured(reading: Reading) -> Sample {
    match reading {
        Reading::Measured(sample) => sample,
        Reading::Unreadable => panic!("expected a measurement, got Unreadable"),
        Reading::Idle => panic!("expected a measurement, got Idle"),
    }
}

use crate::metrics::{publish_ambient_temperature, publish_chip_temperature};
use crate::threshold::{Action, evaluate};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::Handle as ConfigHandle;
use ubos_config::ThermalConfig;
use ubos_core::units::Temperature;
use ubos_logging::{error, info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage, PauseReason};

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

/// What a safety reading measures. The value selects the danger
/// threshold, and `metrics` turns it into the `source` label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The only chip-temperature estimate the board reports. The gauge
    /// carries no `source` label.
    Sole,
    /// A remote diode wired to a chip die. Selects
    /// `dangerous_chip_temp_c`.
    Chip,
    /// A PCB reading plus an offset, published beside a real die
    /// reading. Selects `dangerous_temp_c`.
    Approximated,
}

/// One sample from a board temperature sensor.
pub struct Sample {
    /// The reading that gates thermal shutdown.
    pub safety: Temperature,
    /// What [`safety`](Self::safety) measures.
    pub safety_source: Source,
    /// The reading that drives the cooling PID, when the board reports a
    /// second value. `None` means the PID tracks the safety reading.
    pub pid: Option<Temperature>,
}

impl Sample {
    /// A board that reports one estimate and nothing else. The gauge
    /// carries no label and the PID tracks the same value.
    #[must_use]
    pub const fn sole(safety: Temperature) -> Self {
        Self {
            safety,
            safety_source: Source::Sole,
            pid: None,
        }
    }
}

/// What one poll of a board temperature sensor found.
///
/// The three answers differ in what they oblige the loop to do, which is
/// why silence is two variants and not one.
pub enum Reading {
    /// A usable measurement.
    Measured(Sample),
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

/// A single thermal sample from the board sensor.
///
/// Published by `run_thermal` on every tick that measured something, on
/// every idle tick, and on every unreadable tick past the tolerance
/// budget. Consumers read it through `Handle`.
#[derive(Clone)]
pub struct ThermalReading {
    /// Maximum chip temperature observed. The real die reading where
    /// the board provides one; gates thermal shutdown. Held over from
    /// the last measurement on a tick that measured nothing, so a
    /// consumer reads the last known temperature, and `None` until the
    /// first measurement.
    pub chip_max: Option<Temperature>,
    /// Temperature that drives the cooling PID. PCB-plus-offset
    /// estimate rather than the real die reading, falling back to the
    /// safety reading when the board reports no distinct estimate.
    /// `None` on a tick that measured nothing: [`chip_max`](Self::chip_max)
    /// is held over for the operator, this one is not, because the fan
    /// holds full on it rather than on a value never measured.
    pub pid_temp: Option<Temperature>,
    /// The ambient temperature around the board, where the board measures
    /// it. What it is and is not read for: [`AmbientSensor`].
    pub ambient: Option<Temperature>,
    /// Chip-derived voltage sum, separate from a measured PSU output rail.
    /// It goes out only beside a measurement, so a tick that measured
    /// nothing reports `None` here as in [`pid_temp`](Self::pid_temp),
    /// and the value is never held over.
    pub chain_voltage_mv: Option<u32>,
    /// Monotonic timestamp of this reading in milliseconds since boot.
    pub timestamp_ms: u64,
}

impl ThermalReading {
    /// Whether this tick measured nothing, so the chip temperature it
    /// carries is held over from an earlier one. Derived from
    /// [`pid_temp`](Self::pid_temp), which is the one field that is
    /// never held over; a consumer that wants the PID input reads that
    /// field for its value, not this answer.
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
        self.pid_temp.is_none()
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

/// Publish `reading` directly onto the thermal watch, bypassing
/// `run_thermal`. Lets a host test in a consuming crate drive a reading
/// without standing up a real sensor task.
#[cfg(feature = "test-util")]
pub fn publish_reading_for_test(reading: ThermalReading) {
    STATE_WATCH.sender().send(reading);
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

/// Publish the safety reading, and the cooling-PID estimate too when the
/// board reports a distinct one, each tagged with its source. Returns the
/// value the cooling PID should track: the estimate when present, else
/// the safety reading.
async fn publish_chip_temperatures(sample: &Sample) -> Temperature {
    publish_chip_temperature(sample.safety, sample.safety_source).await;
    match sample.pid {
        Some(approximated) => {
            publish_chip_temperature(approximated, Source::Approximated).await;
            approximated
        }
        None => sample.safety,
    }
}

/// Consecutive [`Reading::Unreadable`] answers, and the halt this loop
/// owes the manager once they run past [`SENSOR_FAULT_TICKS`].
///
/// The count is the budget and nothing more. Whether the halt is held
/// is the manager's mask to say, read back the same way the overheat
/// halt is, so no copy of it lives here to fall out of step. The message
/// pair is asymmetric — only the sender of
/// [`ManagerMessage::TemperatureUnreadable`] may retract it, and it
/// must — so both directions live here rather than one of them being
/// spelled at a call site.
#[derive(Default)]
struct UnreadableHalt {
    ticks: u32,
}

impl UnreadableHalt {
    /// Count one unreadable answer, and ask the manager to halt once
    /// the budget is spent and the halt is not yet held. What the loop
    /// does with a spent budget is [`Self::budget_spent`]'s caller to
    /// decide.
    async fn miss(&mut self, manager_handle: &ManagerHandle) {
        self.ticks = self.ticks.saturating_add(1);
        if !self.budget_spent() {
            return;
        }
        if manager_handle.holds(PauseReason::TemperatureUnreadable) {
            return;
        }
        crate::events::sensor_fault(self.ticks);
        error!(
            "thermal: sensor unreadable for {} ticks; halting + forcing fan to 100%",
            self.ticks
        );
        manager_handle
            .send(ManagerMessage::TemperatureUnreadable)
            .await;
    }

    /// Whether the tolerance budget is spent, so that the loop and the
    /// halt read the same threshold from one place.
    fn budget_spent(&self) -> bool {
        self.ticks >= SENSOR_FAULT_TICKS
    }

    /// Reset the count, and release a halt asked for earlier. Answers
    /// the misses the released halt had counted, and `None` when there
    /// was no halt to release, so the caller reads the mask once, here.
    ///
    /// Called both when a reading returns and when the board goes idle,
    /// because either one ends the condition that halted mining. The idle
    /// case is the one that matters, for the reason `docs/MINING.md`
    /// gives under "Chain presence": a halt that waited for a reading to
    /// release it would never release at all.
    async fn clear(&mut self, manager_handle: &ManagerHandle) -> Option<u32> {
        let misses = core::mem::take(&mut self.ticks);
        if !manager_handle.holds(PauseReason::TemperatureUnreadable) {
            return None;
        }
        manager_handle
            .send(ManagerMessage::TemperatureReadable)
            .await;
        Some(misses)
    }
}

/// What a tick decided from the sensor's answer: whether a reading goes
/// out, and the two temperatures it carries.
#[derive(Debug, PartialEq, Eq)]
enum Tick {
    /// Publish a reading. The chip temperature is this tick's, or the
    /// last one measured on a tick that measured nothing. The PID
    /// temperature is `None` on such a tick, which is what tells a
    /// consumer the chip temperature beside it is held over.
    Publish {
        chip_max: Option<Temperature>,
        pid_temp: Option<Temperature>,
    },
    /// Inside the miss budget: say nothing this tick. Silence that short
    /// is a hiccup the loop rides out, and a reading for it would park
    /// the fan at full for a glitch.
    Withhold,
}

/// What one tick hands the next: the miss budget, and the last chip
/// temperature measured.
#[derive(Default)]
struct Thermal {
    unreadable_halt: UnreadableHalt,
    /// The last chip temperature measured, published again on a tick
    /// that measured nothing, so a consumer in such a gap reads the last
    /// known temperature. `None` until the first measurement.
    last_chip_max: Option<Temperature>,
}

impl Thermal {
    /// Decide one tick from the sensor's answer: count or clear the
    /// miss budget, ask the manager for the halts and releases the
    /// answer calls for, and say what to publish. The manager sends and
    /// the chip gauge push are the decision's effects, which is why it
    /// awaits; the sensor poll and the reading that goes out are the
    /// loop's.
    async fn judge(
        &mut self,
        reading: Reading,
        config: &ThermalConfig,
        manager_handle: &ManagerHandle,
    ) -> Tick {
        match reading {
            Reading::Measured(sample) => {
                if let Some(misses) = self.unreadable_halt.clear(manager_handle).await {
                    crate::events::sensor_recovered(misses);
                    info!("thermal: sensor recovered after {} misses", misses);
                }
                self.last_chip_max = Some(sample.safety);
                let pid_temp = publish_chip_temperatures(&sample).await;
                judge_dangerous(&sample, config, manager_handle).await;
                Tick::Publish {
                    chip_max: self.last_chip_max,
                    pid_temp: Some(pid_temp),
                }
            }
            Reading::Unreadable => {
                // Past the budget the board runs unwatched and every
                // consumer hears so on every tick.
                self.unreadable_halt.miss(manager_handle).await;
                if self.unreadable_halt.budget_spent() {
                    self.held()
                } else {
                    Tick::Withhold
                }
            }
            Reading::Idle => {
                // Why an idle board releases the halt: "Chain presence" in
                // `docs/MINING.md`. It has nothing to measure, so every
                // consumer hears on the first idle tick that the reading
                // is held.
                if self.unreadable_halt.clear(manager_handle).await.is_some() {
                    info!("thermal: sensor owes no reading, releasing the halt");
                }
                self.held()
            }
        }
    }

    /// The reading a tick that measured nothing publishes: the last chip
    /// temperature, and no PID temperature beside it.
    fn held(&self) -> Tick {
        Tick::Publish {
            chip_max: self.last_chip_max,
            pid_temp: None,
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
        publish_ambient_temperature(reading).await;
    }
    reading
}

/// Run the thermal monitoring loop forever.
///
/// Spawned once per board firmware. `sensor` is the board-specific
/// temperature sensor driver and `ambient` its ambient temperature
/// sensor, [`NoAmbient`] where it has none. `manager_handle` takes the two
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
pub async fn run_thermal<S: TemperatureSensor, A: AmbientSensor>(
    mut sensor: S,
    mut ambient: A,
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

        let tick = thermal
            .judge(sensor.sample().await, &config, &manager_handle)
            .await;
        let ambient_temp = read_ambient(&mut ambient).await;
        let Tick::Publish { chip_max, pid_temp } = tick else {
            continue;
        };
        // The chain voltage is not held over the way the chip
        // temperature is: it goes out only beside a measurement, and a
        // tick that measured nothing reports none.
        publisher.send(ThermalReading {
            chip_max,
            pid_temp,
            ambient: ambient_temp,
            chain_voltage_mv: pid_temp.and_then(|_| sensor.chain_voltage_mv()),
            timestamp_ms: now.as_millis(),
        });
    }
}

/// Task shell around [`run_thermal`], which polls the temperature
/// sensors and publishes the thermal reading. Takes the board's sensor
/// type, an I2C sensor array on most boards, and its ambient temperature sensor type,
/// [`NoAmbient`] on a board without one. The ambient type has no default, for
/// the reason [`AmbientSensor`] gives.
#[macro_export]
macro_rules! thermal_task {
    ($sensor:ty, $ambient:ty) => {
        #[embassy_executor::task]
        async fn thermal_task(
            sensor: $sensor,
            ambient: $ambient,
            manager_handle: ubos_manager::Handle,
        ) -> ! {
            ubos_thermal::run_thermal(sensor, ambient, manager_handle, ubos_config::handle()).await
        }
    };
}

/// Judge one measurement against the thresholds, and ask the manager
/// for the halt or the release the crossing calls for.
///
/// The counterpart of [`UnreadableHalt`], owning both directions of its
/// message pair for the same reason. It is a function and not a struct
/// because this halt has no state of its own: the threshold decision is
/// [`evaluate`]'s, and whether the halt stands is the manager's mask to
/// say. Reading the mask back each tick is what makes an operator's
/// release real: a local flag would leave this loop halted for ever
/// after, with `evaluate` answering `Ok` at any temperature.
async fn judge_dangerous(sample: &Sample, config: &ThermalConfig, manager_handle: &ManagerHandle) {
    let dangerous = match sample.safety_source {
        Source::Chip => config.dangerous_chip_temp_c,
        Source::Sole | Source::Approximated => config.dangerous_temp_c,
    };
    let halted = manager_handle.holds(PauseReason::DangerousTemperature);
    let temp = sample.safety;
    match evaluate(
        temp.celsius_f32(),
        halted,
        dangerous,
        config.cooldown_temp_c,
    ) {
        Action::Ok => {}
        Action::EnterHalt => {
            manager_handle
                .send(ManagerMessage::DangerousTemperature)
                .await;
            error!(
                "thermal: dangerous temperature {}°C, halting mining",
                temp.celsius_f32()
            );
            crate::events::critical_temperature(temp_celsius_i16(temp));
        }
        Action::Resume => {
            manager_handle.send(ManagerMessage::TemperatureNormal).await;
            warn!(
                "thermal: cooled to {}°C, resuming mining",
                temp.celsius_f32()
            );
            crate::events::temperature_recovered(temp_celsius_i16(temp));
        }
    }
}
