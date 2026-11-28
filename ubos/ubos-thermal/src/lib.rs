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

//! Thermal task: takes the temperature readings of every source, writes
//! one metric for each, filters them, and publishes the filtered
//! temperatures and the dangerous temperature flag. Thermal measures and
//! publishes, and the one decision it makes is the flag. A consumer
//! decides what any other value means, and what `None` means.
//! `ubos-mining` halts the machine while the flag is set, see "The
//! overheat halt" in `docs/MINING.md`.
//!
//! A source hands each reading to the task through [`Handle::submit`]
//! and judges none of them, and the driver of a chain hands over the die
//! of a chip through [`Handle::submit_chip`] when the chip answers. The
//! task writes the series of that reading in `metrics`, filters the
//! readings by kind, and publishes the filtered temperatures once on each
//! tick. The API reads them through [`Handle::filtered`] and
//! [`Handle::ambient`]. Cooling subscribes to the one temperature that
//! its PID follows, through [`Handle::subscribe_pid_temperature`].
//!
//! [`ChainTemperature`] is the sensor of a board whose chips are its
//! thermometer. It also keeps the hottest die and the voltage sum of
//! each chain, which the API and the tuner read through
//! [`Handle::chains`], and it asks the miner which chains are present
//! through [`PresentChains`].
//!
//! A sensor of a board with nothing powered submits nothing, which is no
//! fault: "Chain presence" in `docs/MINING.md`. This task also
//! writes the series of the ambient sensor, and the MCU temperature
//! gauge, which it reads from the board's [`McuTemperatureSensor`] on
//! each tick. `array` reads a board's I2C sensor table through the
//! drivers and the descriptor types in `ubos-temperature`.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

#[cfg(test)]
extern crate std;

pub mod chain;
pub use chain::{ChainTemperature, PresentChains};
mod array;
mod chain_readings;
mod danger;
mod filter;
mod metrics;
mod stats;
mod temps;

pub use array::SensorArray;
pub use chain_readings::ChainReadings;
pub use stats::{Accumulator, TempStats};
pub use temps::PidTemperature;

#[cfg(test)]
#[path = "tests/run_thermal.rs"]
mod tests_run_thermal;

/// Serialises every test that reads the mock clock: they share the
/// clock, the config watch and the global gauge channel.
#[cfg(test)]
static LOOP_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A poisoned lock only means an earlier test panicked.
#[cfg(test)]
fn clock() -> std::sync::MutexGuard<'static, ()> {
    LOOP_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The config that a first boot builds for the hashboard the tests run
/// on.
#[cfg(test)]
fn default_config() -> ubos_config::Config {
    ubos_config::Config::for_board(
        &ubos_asic::BMM101_HASHBOARD.defaults,
        ubos_config::DefaultTuning::Fixed,
    )
}

use crate::danger::Danger;
use crate::metrics::publish_reading;
use crate::temps::{Temperatures, Watches};
use core::convert::Infallible;

use embassy_futures::select::{Either, select};
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::Receiver;
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::Handle as ConfigHandle;
use ubos_config::{PidTarget, TemperatureDescriptor, ThermalConfig};
use ubos_core::index::{ChipAddress, HashboardIndex};
use ubos_core::units::Temperature;
use ubos_logging::{info, warn};
use ubos_metrics::metric;
use ubos_temperature::{Reading as TempReading, TempSource};

ubos_events::events! {
    dangerous_temperature(temp: Temperature)  error = "Critical temperature {} °C, mining halted",
    temperature_cooled(temp: Temperature)     info  = "Temperature recovered to {} °C, resuming",
}

type CSMutex = CriticalSectionRawMutex;

/// What a reading measures.
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
}

/// Board temperature sensor. One [`sample`](Self::sample) reads the
/// sensors and hands each reading to thermal through its [`Handle`]. It
/// answers nothing: a sensor with nothing to read submits nothing, and
/// each consumer of the published temperatures decides what that means.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    async fn sample(&mut self);
}

/// The ambient temperature sensor. There is no default, so a board
/// states that it has none by handing [`NoAmbient`].
///
/// The reading is for the operator and the metrics. Nothing in this
/// crate decides on it: it gates no halt and drives no fan. This is the
/// one statement of that; every other mention points here.
#[allow(async_fn_in_trait)]
pub trait AmbientSensor {
    /// `None` when the read failed. The wait is the implementor's to
    /// bound: the loop awaits this unbounded, so a sensor that never
    /// answers ends the ticks and every judgement they carry.
    async fn ambient(&mut self) -> Option<Temperature>;
}

/// The ambient sensor of a board that has none.
pub struct NoAmbient;

impl AmbientSensor for NoAmbient {
    async fn ambient(&mut self) -> Option<Temperature> {
        None
    }
}

/// The thermometer on the MCU die, which measures the control board. A board
/// whose MCU has none hands [`NoMcu`]. The reading feeds its series and no
/// control loop.
#[allow(async_fn_in_trait)]
pub trait McuTemperatureSensor {
    /// `None` if the reading failed.
    async fn read(&mut self) -> Option<Temperature>;
}

/// Reads `None`, so its series is never written.
pub struct NoMcu;

impl McuTemperatureSensor for NoMcu {
    async fn read(&mut self) -> Option<Temperature> {
        None
    }
}

/// Subscriber capacity of each thermal watch. Four receivers cover
/// cooling, the API, the display and mining without enlarging a watch
/// further for today's consumers.
const WATCH_CAPACITY: usize = 4;

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("thermal");

/// Depth of the input channel. [`run_thermal`] drains it at every await
/// of its loop, so the depth holds what the drivers submit between two
/// polls of the thermal task. A chip that finds it full is dropped, see
/// [`Handle::submit_chip`].
const INPUT_DEPTH: usize = 64;

/// What a source hands to thermal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Submitted {
    /// The reading of one sensor. It gets a series of its own and goes
    /// into the filter of its kind.
    Reading(TempReading),
    /// The die of one chip of a chain, handed in by the driver when the
    /// chip answers. It gets a series of its own, and the filter takes
    /// the dies of the chain from [`Submitted::Dies`].
    Chip(TempReading),
    /// The hottest and the mean die of the chains of one tick. It goes
    /// into the chip filter, because a chip answers once in a pass and
    /// the filter needs the dies of every tick.
    Dies(TempStats),
}

/// What sources hand to thermal, through the submit methods of
/// [`Handle`]. [`run_thermal`] is the only reader.
static INPUT: Channel<CSMutex, Submitted, INPUT_DEPTH> = Channel::new();

/// The filtered temperatures the firmware's one thermal task publishes.
static WATCHES: Watches = Watches::new();

/// The operator's acknowledge of the dangerous temperature flag, from
/// [`Handle::acknowledge`]. The loop takes it on its next tick.
static ACKNOWLEDGE: Signal<CSMutex, ()> = Signal::new();

/// Per-task subscription returned by
/// [`Handle::subscribe_pid_temperature`].
pub type PidTemperatureSubscription = Receiver<'static, CSMutex, PidTemperature, WATCH_CAPACITY>;

/// Per-task subscription returned by [`Handle::subscribe_dangerous`].
pub type FlagSubscription = Receiver<'static, CSMutex, bool, WATCH_CAPACITY>;

/// Consumer-side handle for the thermal subsystem.
///
/// The handle holds no data: its methods are the only way to the private
/// statics of this module. A firmware takes it once and passes a copy to
/// each task that uses it.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the task that reads the sensors.
///
/// It is not `Copy`, so a firmware that gives it to one task cannot give
/// it to a second.
pub struct Runner(());

/// What the thermal task does to its statics, for a test that drives a
/// consumer of them with no thermal task of its own. These are methods of
/// the runner because its holder is the one writer of the watches.
#[cfg(feature = "test-util")]
impl Runner {
    /// Publish the dangerous temperature flag.
    pub fn publish_dangerous(&self, set: bool) {
        WATCHES.danger.sender().send(set);
    }

    /// Publish the chip and hashboard temperatures, or `None` for no
    /// reading inside the window.
    pub fn publish_filtered(&self, filtered: Option<TempSample>) {
        WATCHES.filtered.sender().send(filtered);
    }

    /// Take the operator's acknowledge as a tick does, and answer whether
    /// one waited.
    pub fn take_acknowledge(&self) -> bool {
        ACKNOWLEDGE.try_take().is_some()
    }
}

impl Handle {
    /// The firmware's one thermal handle, and the runner of the task that
    /// reads the sensors. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
    }

    /// The chip and hashboard temperatures, or `None` when neither kind has
    /// a reading inside the window.
    #[must_use]
    pub fn filtered(&self) -> Option<TempSample> {
        WATCHES.filtered.try_get().flatten()
    }

    /// Whether thermal published that neither kind has a reading inside the
    /// window. A thermal task that has not published yet reads as `false`.
    #[must_use]
    pub fn has_no_reading(&self) -> bool {
        WATCHES.filtered.try_get() == Some(None)
    }

    /// The hottest die and the voltage sum of each chain, as the chain
    /// sensor last read them.
    #[must_use]
    pub fn chains(&self) -> ChainReadings {
        ChainReadings::board()
    }

    /// The air around the board, or `None` when no ambient reading is
    /// inside the window. A board without an ambient sensor always answers
    /// `None`, and so does every board before the first tick.
    #[must_use]
    pub fn ambient(&self) -> Option<Temperature> {
        WATCHES.ambient.try_get().flatten()
    }

    /// Per-task subscription to the temperature that the cooling PID
    /// follows: the statistic of the cooling target over the readings of
    /// its kind, or `None` when that kind has no reading inside the
    /// window, beside the kind and the statistic that selected it. The
    /// loop sends it on every tick, so a subscriber that
    /// stops waking knows that the loop stopped. Call once per consumer
    /// task before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the watch has no free receiver slots.
    #[must_use]
    pub fn subscribe_pid_temperature(&self) -> PidTemperatureSubscription {
        WATCHES
            .pid
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Whether the dangerous temperature flag is set. A thermal task that
    /// has not published yet reads as clear.
    #[must_use]
    pub fn is_dangerous(&self) -> bool {
        WATCHES.danger.try_get() == Some(true)
    }

    /// Per-task subscription to the dangerous temperature flag. Call once
    /// per consumer task before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the watch has no free receiver slots.
    #[must_use]
    pub fn subscribe_dangerous(&self) -> FlagSubscription {
        WATCHES
            .danger
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Clear the flag on the operator's word. Thermal takes the
    /// acknowledge on its next tick, and a reading that reaches its limit
    /// afterwards sets the flag again.
    pub fn acknowledge(&self) {
        ACKNOWLEDGE.signal(());
    }

    /// Hand one reading to thermal. Waits while the input channel is full.
    pub async fn submit(&self, reading: TempReading) {
        INPUT.send(Submitted::Reading(reading)).await;
    }

    /// Hand the die temperature that `chip` of `chain` just reported to
    /// thermal, which writes its series. The driver of the chain calls
    /// this from its serial actor, so it returns at once: a reading that
    /// finds the input channel full is dropped, and the chip submits
    /// again on its next turn.
    pub fn submit_chip(&self, chain: HashboardIndex, chip: ChipAddress, value: Temperature) {
        let source = TempSource::chip(chain, chip);
        let _ = INPUT.try_send(Submitted::Chip(TempReading { source, value }));
    }

    /// Hand the hottest and the mean die of the chains of this tick to
    /// thermal, for the chip filter. Waits while the input channel is
    /// full.
    pub(crate) async fn submit_dies(&self, dies: TempStats) {
        INPUT.send(Submitted::Dies(dies)).await;
    }
}

/// What the loop folds each submitted input into.
struct Folds {
    temperatures: Temperatures,
    danger: Danger,
}

impl Folds {
    const fn new(interval: Duration, dangerous: TemperatureDescriptor) -> Self {
        Self {
            temperatures: Temperatures::for_interval(interval),
            danger: Danger::new(&WATCHES.danger, dangerous),
        }
    }
}

/// Take everything that a source submits as input of the tick at
/// `tick`, and never return. A sensor awaits on a full channel and a
/// driver submits a chip at any moment, so this runs beside every await
/// of the loop, through [`draining`]. It yields after each input, so
/// the ingest task drains the metrics channel while a backlog of chips is
/// written.
async fn drain_input(folds: &mut Folds, tick: Instant) -> Infallible {
    loop {
        take_submitted(INPUT.receive().await, folds, tick);
        yield_now().await;
    }
}

/// Await `work` and answer what it gives, taking the input of the tick
/// at `tick` for as long as it runs. A chain driver submits its chips
/// faster than the channel is deep while the loop waits on a sensor, so
/// every await of the loop goes through here.
async fn draining<T>(work: impl Future<Output = T>, folds: &mut Folds, tick: Instant) -> T {
    match select(work, drain_input(folds, tick)).await {
        Either::First(output) => output,
        Either::Second(never) => match never {},
    }
}

/// Give `submitted` its series, its place in a filter at `tick`, or
/// both, as its variant says, and check it against the limit of its
/// kind. The check is here so that a hot reading sets the flag when it
/// arrives and not on the next tick.
fn take_submitted(submitted: Submitted, folds: &mut Folds, tick: Instant) {
    match submitted {
        Submitted::Reading(reading) => take(reading, folds, tick),
        Submitted::Chip(reading) => {
            publish_reading(reading);
            folds.danger.on_reading(reading);
        }
        Submitted::Dies(dies) => {
            folds.temperatures.update_dies(dies, tick);
            folds.danger.on_value(Source::Chip, dies.max);
        }
    }
}

/// Write the metric of a submitted reading, fold it into the filters at
/// `tick`, the time of the tick that took it, and check it against its
/// limit. A sample can take longer than the window of a filter, so a
/// reading stamped with the time it arrived at could leave the window
/// before its own tick publishes.
fn take(reading: TempReading, folds: &mut Folds, tick: Instant) {
    publish_reading(reading);
    folds.temperatures.update(reading, tick);
    folds.danger.on_reading(reading);
}

/// Read the ambient temperature and take it as a submitted reading. It gates
/// nothing, so it is read after every decision a tick makes: a slow sensor
/// must not hold up the sample the halt turns on. Every tick reads it, since
/// the chip sensor's silence says nothing about this one.
async fn read_ambient<A: AmbientSensor>(ambient: &mut A, folds: &mut Folds, tick: Instant) {
    if let Some(value) = draining(ambient.ambient(), folds, tick).await {
        take(
            TempReading {
                source: TempSource::Ambient,
                value,
            },
            folds,
            tick,
        );
    }
}

/// Run the thermal monitoring loop forever.
///
/// `mcu` is the MCU die thermometer, whose reading goes to the metrics on each
/// tick. Each submitted reading goes to the metrics and the filters, and each
/// tick publishes the filtered temperatures and checks the dangerous
/// temperature flag. `_runner` is borrowed mutably, so one runner runs one
/// loop at a time.
pub async fn run_thermal<S: TemperatureSensor, A: AmbientSensor, M: McuTemperatureSensor>(
    mut sensor: S,
    mut ambient: A,
    mut mcu: M,
    _runner: &mut Runner,
    config_handle: ConfigHandle,
) -> ! {
    let mut config_subscription = config_handle.subscribe();
    let current = config_handle.current().await;
    let mut config: ThermalConfig = current.thermal;
    let mut pid_target: PidTarget = current.cooling.target;

    let mut sample_interval = Duration::from_millis(u64::from(config.sample_interval_ms));
    let mut ticker = Ticker::every(sample_interval);
    let mut late_threshold = sample_interval * 2;

    let mut folds = Folds::new(sample_interval, config.dangerous);
    folds.danger.publish();
    let mut prev_tick = Instant::now();

    info!("thermal: started");

    loop {
        draining(ticker.next(), &mut folds, prev_tick).await;
        if let Some(updated) = config_subscription.try_changed() {
            let new_thermal = updated.thermal;
            if new_thermal.sample_interval_ms != config.sample_interval_ms {
                sample_interval = Duration::from_millis(u64::from(new_thermal.sample_interval_ms));
                ticker = Ticker::every(sample_interval);
                folds.temperatures = Temperatures::for_interval(sample_interval);
                late_threshold = sample_interval * 2;
                prev_tick = Instant::now();
                info!(
                    "thermal: sample interval updated to {} ms",
                    new_thermal.sample_interval_ms
                );
            }
            config = new_thermal;
            folds.danger.set_limits(config.dangerous);
            pid_target = updated.cooling.target;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(prev_tick);
        if elapsed > late_threshold {
            warn!("thermal: tick late by {} ms", elapsed.as_millis());
        }
        prev_tick = now;

        draining(sensor.sample(), &mut folds, now).await;
        // The sample left its last readings queued, and this tick publishes
        // them. The count is taken once, so chips that keep arriving wait
        // for the drain of the next await and the publish starts on time.
        for _ in 0..INPUT.len() {
            let Ok(submitted) = INPUT.try_receive() else {
                break;
            };
            take_submitted(submitted, &mut folds, now);
            yield_now().await;
        }
        // The readings carry the time of this tick, so the filters are
        // read at that time and hold every reading of this tick, however
        // long the sample took.
        folds.temperatures.publish_filtered(now, &WATCHES);
        folds.temperatures.publish_pid(now, pid_target, &WATCHES);
        if ACKNOWLEDGE.try_take().is_some() {
            folds.danger.acknowledge();
        }
        folds
            .danger
            .on_tick(&folds.temperatures, config.cooldown_temp, now);
        read_ambient(&mut ambient, &mut folds, now).await;
        folds.temperatures.publish_ambient(now, &WATCHES);
        // Read last, after the sample the cooling loop turns on has gone out,
        // and dropped on a full channel: this safety loop must not wait on the
        // metrics.
        if let Some(temp) = draining(mcu.read(), &mut folds, now).await {
            metric!("mcu_temperature_celsius", temp).try_push();
        }
    }
}

/// Task shell around [`run_thermal`]. Neither the ambient nor the MCU sensor
/// type has a default, for the reason [`AmbientSensor`] gives.
#[macro_export]
macro_rules! thermal_task {
    ($sensor:ty, $ambient:ty, $mcu:ty) => {
        #[embassy_executor::task]
        async fn thermal_task(
            sensor: $sensor,
            ambient: $ambient,
            mcu: $mcu,
            mut runner: ubos_thermal::Runner,
            config_handle: ubos_config::Handle,
        ) -> ! {
            ubos_thermal::run_thermal(sensor, ambient, mcu, &mut runner, config_handle).await
        }
    };
}
