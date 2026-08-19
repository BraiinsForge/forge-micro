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
//! the chip temperature gauge, in `metrics`.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod metrics;
mod threshold;

#[cfg(test)]
#[path = "tests/threshold.rs"]
mod tests_threshold;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Ticker};
use ubos_config::ThermalConfig;
use ubos_core::units::Temperature;
use ubos_logging::{error, info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_temperature::{CHIP_SOURCE, TemperatureSensor};

use crate::metrics::publish_chip_temperature;
use crate::threshold::{Action, evaluate};

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

/// Module-internal state watch. Single writer is [`run_thermal`];
/// readers obtained via [`Handle::subscribe`].
pub(crate) static STATE_WATCH: Watch<CSMutex, ThermalReading, WATCH_CAPACITY> = Watch::new();

/// Per-task subscription returned by [`Handle::subscribe`].
pub type ThermalSubscription = Receiver<'static, CSMutex, ThermalReading, WATCH_CAPACITY>;

/// Obtain a handle to the thermal module. `Copy`; callers freely pass
/// it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
    }
}

/// Consumer-side handle for the thermal subsystem.
///
/// `Copy` because it holds only a `&'static` reference. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, ThermalReading, WATCH_CAPACITY>,
}

impl Handle {
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
/// board exposes a distinct one, each tagged with its source label. Returns
/// the value the cooling PID should track: the estimate when present, else
/// the safety reading.
async fn publish_chip_temperatures<S: TemperatureSensor>(
    sensor: &S,
    safety: Temperature,
) -> Temperature {
    publish_chip_temperature(safety, sensor.temperature_source()).await;
    match sensor.pid_temperature() {
        Some(approximated) => {
            publish_chip_temperature(approximated, sensor.pid_temperature_source()).await;
            approximated
        }
        None => safety,
    }
}

/// Run the thermal monitoring loop forever.
///
/// Spawned once per board firmware. `sensor` is the board-specific
/// temperature sensor driver. `manager` is the handle used to send
/// `ManagerMessage::DangerousTemperature` and
/// `ManagerMessage::TemperatureNormal` on threshold and recovery edges.
/// The manager is the sole caller of `MiningHandle::halt`/`resume`.
/// Thermal tuning is read from [`ubos_config::Handle`] and refreshed
/// whenever the live config publishes a new value.
pub async fn run_thermal<S: TemperatureSensor>(mut sensor: S, manager: ManagerHandle) -> ! {
    let publisher = STATE_WATCH.sender();
    let config_handle = ubos_config::handle();
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

        let Some(temp) = sensor.read().await else {
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
        last_chip_max = Some(temp);
        let pid_temp = publish_chip_temperatures(&sensor, temp).await;
        last_pid_temp = Some(pid_temp);

        let dangerous = if sensor.temperature_source() == Some(CHIP_SOURCE) {
            config.dangerous_chip_temp_c
        } else {
            config.dangerous_temp_c
        };
        let action = evaluate(
            temp.celsius_f32(),
            halted,
            dangerous,
            config.cooldown_temp_c,
        );
        apply_action(action, &mut halted, temp, &manager).await;

        publisher.send(ThermalReading {
            chip_max: temp,
            pid_temp,
            timestamp_ms: now.as_millis(),
            sensor_fault: false,
        });
    }
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
