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
//! `ThermalReading` via `Handle`. Sends
//! `ManagerMessage::DangerousTemperature` and `TemperatureNormal` on
//! threshold and recovery edges; the manager is the sole caller of
//! `MiningHandle::halt`/`resume`. This task is also the sole writer of
//! the chip temperature gauge, in `metrics`. `array` aggregates a
//! board's I2C sensor table into the reading the task polls, over the
//! drivers and the descriptor types in `ubos-temperature`.

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

use crate::metrics::publish_chip_temperature;
use crate::threshold::{Action, evaluate};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::Handle as ConfigHandle;
use ubos_config::ThermalConfig;
use ubos_core::units::Temperature;
use ubos_logging::{error, info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};

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

/// Board temperature sensor. [`sample`](Self::sample) returns everything
/// one tick needs, so no reading is cached between calls and no call
/// order is implied.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Take one sample, or `None` if the reading failed.
    /// Implementations may await on the underlying hardware.
    async fn sample(&mut self) -> Option<Sample>;

    /// Sum of chip core voltages when this sensor also observes the chain.
    /// `None` means the sensor cannot provide a complete chain measurement.
    fn chain_voltage_mv(&self) -> Option<u32> {
        None
    }

    /// Whether a sensor that has stopped reading should halt mining.
    ///
    /// True for a board whose sensor is independent of what it measures,
    /// where silence means the reading is gone and the board is running
    /// unwatched. False for one whose only thermometer is inside the
    /// chips it is trying to bring up, where silence is the normal state
    /// until the chain answers and halting on it stops the very work that
    /// would produce a reading.
    ///
    /// A board that returns false still logs the fault, still publishes
    /// it, and still gets the fan forced to full. It just is not stopped.
    ///
    /// This is a fact about the sensor and not about any one reading, so
    /// it stays a method rather than a [`Sample`] field: the fault path
    /// asks it exactly when there is no sample to carry it.
    fn halts_on_fault(&self) -> bool {
        true
    }
}

/// A single thermal sample from the board sensor.
///
/// Published by `run_thermal` at every successful sensor read and on every
/// sensor-fault transition. Consumers read it through `Handle`.
#[derive(Clone)]
pub struct ThermalReading {
    /// Maximum chip temperature observed in this sample. The real die
    /// reading where the board provides one; gates thermal shutdown.
    pub chip_max: Temperature,
    /// Temperature that drives the cooling PID. PCB-plus-
    /// offset estimate rather than the real die reading.
    pub pid_temp: Temperature,
    /// Chip-derived voltage sum, separate from a measured PSU output rail.
    pub chain_voltage_mv: Option<u32>,
    /// Monotonic timestamp of this reading in milliseconds since boot.
    pub timestamp_ms: u64,
    /// True when the sensor has been unreadable for `SENSOR_FAULT_TICKS`
    /// consecutive ticks. Cleared on the first successful read thereafter.
    pub sensor_fault: bool,
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

/// Consecutive sensor-read failures tolerated before declaring the bus
/// dead. At a 1 Hz sample interval this is 5 seconds, long enough to
/// ride out a transient I2C glitch but short enough to react if the bus
/// has gone permanently silent (stuck SDA, dead chip, broken cable). On
/// fault the loop halts mining and parks the fan at 100% until a
/// successful reading clears it.
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

/// Run the thermal monitoring loop forever.
///
/// Spawned once per board firmware. `sensor` is the board-specific
/// temperature sensor driver. `manager` is the handle used to send
/// `ManagerMessage::DangerousTemperature` and
/// `ManagerMessage::TemperatureNormal` on threshold and recovery edges.
/// The manager is the sole caller of `MiningHandle::halt`/`resume`.
/// Thermal tuning is read from `config` and refreshed whenever that
/// config publishes a new value. Board firmware passes
/// [`ubos_config::handle`], which [`thermal_task`] supplies for it.
pub async fn run_thermal<S: TemperatureSensor>(
    mut sensor: S,
    manager: ManagerHandle,
    config_handle: ConfigHandle,
) -> ! {
    let publisher = STATE_WATCH.sender();
    let mut config_subscription = config_handle.subscribe();
    let mut config: ThermalConfig = config_handle.current().thermal;

    let mut sample_interval = Duration::from_millis(u64::from(config.sample_interval_ms));
    let mut ticker = Ticker::every(sample_interval);
    let mut late_threshold = sample_interval * 2;

    let mut halted = false;
    let mut sensor_misses: u32 = 0;
    let mut prev_tick = Instant::now();
    // Carries forward across a sensor-fault gap so consumers reading
    // `current()` between fault ticks see the last known temperature
    // alongside `sensor_fault: true`, not a synthetic 0.
    let mut last_chip_max: Option<Temperature> = None;
    let mut last_pid_temp: Option<Temperature> = None;

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

        let Some(sample) = sensor.sample().await else {
            sensor_misses = sensor_misses.saturating_add(1);
            if sensor_misses == SENSOR_FAULT_TICKS {
                crate::events::sensor_fault(sensor_misses);
                if sensor.halts_on_fault() {
                    error!(
                        "thermal: sensor unreadable for {} ticks; halting + forcing fan to 100%",
                        sensor_misses
                    );
                    halted = true;
                    manager.send(ManagerMessage::DangerousTemperature).await;
                } else {
                    warn!(
                        "thermal: sensor unreadable for {} ticks; forcing fan to 100%, not halting",
                        sensor_misses
                    );
                }
            }
            if sensor_misses >= SENSOR_FAULT_TICKS {
                publisher.send(ThermalReading {
                    chip_max: last_chip_max.unwrap_or(Temperature::from_millicelsius(0)),
                    pid_temp: last_pid_temp.unwrap_or(Temperature::from_millicelsius(0)),
                    timestamp_ms: now.as_millis(),
                    sensor_fault: true,
                    chain_voltage_mv: None,
                });
            }
            continue;
        };

        if sensor_misses >= SENSOR_FAULT_TICKS {
            crate::events::sensor_recovered(sensor_misses);
            info!("thermal: sensor recovered after {} misses", sensor_misses);
            // Only the branch that halted may release the halt: a sensor
            // that does not halt on fault never set it, and clearing it
            // here would lift one held for an unrelated reason.
            if sensor.halts_on_fault() {
                halted = false;
                manager.send(ManagerMessage::TemperatureNormal).await;
            }
        }
        sensor_misses = 0;
        last_chip_max = Some(sample.safety);
        let pid_temp = publish_chip_temperatures(&sample).await;
        last_pid_temp = Some(pid_temp);

        let dangerous = match sample.safety_source {
            Source::Chip => config.dangerous_chip_temp_c,
            Source::Sole | Source::Approximated => config.dangerous_temp_c,
        };
        let action = evaluate(
            sample.safety.celsius_f32(),
            halted,
            dangerous,
            config.cooldown_temp_c,
        );
        apply_action(action, &mut halted, sample.safety, &manager).await;

        publisher.send(ThermalReading {
            chip_max: sample.safety,
            pid_temp,
            timestamp_ms: now.as_millis(),
            sensor_fault: false,
            chain_voltage_mv: sensor.chain_voltage_mv(),
        });
    }
}

/// Task shell around [`run_thermal`], which polls the temperature
/// sensors and publishes the thermal reading. Takes the board's sensor
/// type: an I2C sensor array on most boards, a single ambient sensor
/// where that is all there is.
#[macro_export]
macro_rules! thermal_task {
    ($sensor:ty) => {
        #[embassy_executor::task]
        async fn thermal_task(sensor: $sensor, manager: ubos_manager::Handle) -> ! {
            ubos_thermal::run_thermal(sensor, manager, ubos_config::handle()).await
        }
    };
}

/// Toggle the halt state and notify the manager based on the threshold
/// decision. `celsius` is used only for the log lines.
async fn apply_action(
    action: Action,
    halted: &mut bool,
    temp: Temperature,
    manager: &ManagerHandle,
) {
    match action {
        Action::Ok => {}
        Action::EnterHalt => {
            *halted = true;
            manager.send(ManagerMessage::DangerousTemperature).await;
            error!(
                "thermal: dangerous temperature {}°C, halting mining",
                temp.celsius_f32()
            );
            crate::events::critical_temperature(temp_celsius_i16(temp));
        }
        Action::Resume => {
            *halted = false;
            manager.send(ManagerMessage::TemperatureNormal).await;
            warn!(
                "thermal: cooled to {}°C, resuming mining",
                temp.celsius_f32()
            );
            crate::events::temperature_recovered(temp_celsius_i16(temp));
        }
    }
}
