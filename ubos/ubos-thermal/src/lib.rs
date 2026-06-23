//! Thermal sensor task: polls the board temperature sensor and publishes
//! `ThermalReading` via `Handle`. Sends
//! `ManagerMessage::DangerousTemperature` and `TemperatureNormal` on
//! threshold and recovery edges; the manager is the sole caller of
//! `MiningHandle::halt`/`resume`.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

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
use ubos_temperature::{publish_board_temperature, TemperatureSensor};

use crate::threshold::{evaluate, Action};

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
    /// Maximum chip temperature observed in this sample.
    pub chip_max: Temperature,
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

/// Consecutive sensor-read failures tolerated before declaring the bus
/// dead. At a 1 Hz sample interval this is 5 seconds, long enough to
/// ride out a transient I2C glitch but short enough to react if the bus
/// has gone permanently silent (stuck SDA, dead chip, broken cable). On
/// fault the loop halts mining and parks the fan at 100% until a
/// successful reading clears it.
const SENSOR_FAULT_TICKS: u32 = 5;

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
                error!(
                    "thermal: sensor unreadable for {} ticks; halting + forcing fan to 100%",
                    sensor_misses
                );
                crate::events::sensor_fault(sensor_misses);
                halted = true;
                manager.send(ManagerMessage::DangerousTemperature).await;
            }
            if sensor_misses >= SENSOR_FAULT_TICKS {
                publisher.send(ThermalReading {
                    chip_max: last_chip_max.unwrap_or(Temperature::from_millicelsius(0)),
                    timestamp_ms: now.as_millis(),
                    sensor_fault: true,
                });
            }
            continue;
        };

        if sensor_misses >= SENSOR_FAULT_TICKS {
            info!(
                "thermal: sensor recovered after {} misses; releasing halt for re-evaluation",
                sensor_misses
            );
            crate::events::sensor_recovered(sensor_misses);
            halted = false;
            manager.send(ManagerMessage::TemperatureNormal).await;
        }
        sensor_misses = 0;
        last_chip_max = Some(temp);

        publish_board_temperature(temp).await;

        match evaluate(
            temp.celsius_f32(),
            halted,
            config.dangerous_temp_c,
            config.cooldown_temp_c,
        ) {
            Action::Ok => {}
            Action::EnterHalt => {
                halted = true;
                manager.send(ManagerMessage::DangerousTemperature).await;
                error!(
                    "thermal: dangerous temperature {}°C, halting mining",
                    temp.celsius_f32()
                );
                crate::events::critical_temperature(temp_celsius_i16(temp));
            }
            Action::Resume => {
                halted = false;
                manager.send(ManagerMessage::TemperatureNormal).await;
                warn!(
                    "thermal: cooled to {}°C, resuming mining",
                    temp.celsius_f32()
                );
                crate::events::temperature_recovered(temp_celsius_i16(temp));
            }
        }

        publisher.send(ThermalReading {
            chip_max: temp,
            timestamp_ms: now.as_millis(),
            sensor_fault: false,
        });
    }
}
