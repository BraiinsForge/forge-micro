//! Board-temperature sensor trait, periodic sampler task, and shared
//! constants for the Prometheus temperature gauge. Hardware drivers
//! (LM75A, etc.) live in sibling modules of this crate; board firmware
//! crates supply their own die-sensor `TemperatureSensor` impl.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod lm75a;

use embassy_time::{Duration, Ticker};
use ubos_core::units::Temperature;
use ubos_metrics::metrics::{push, Metric, Value};

/// Internal chip temperature sampling parameters.
pub struct TemperatureParams;
impl TemperatureParams {
    pub const PERIOD: Duration = Duration::from_secs(1);
    /// Prometheus convention prefers base SI units, which would make
    /// `_celsius` the canonical suffix. This crate targets the RP2040
    /// (Cortex-M0+, no FPU) among others, so a float-valued celsius
    /// gauge would pull software floating-point into the exposition
    /// path. Integer millicelsius preserves sub-degree resolution
    /// without soft-float, and the name reflects the unit carried on
    /// the wire.
    pub const METRIC_NAME: &str = "board_chip_temperature_millicelsius";
}

/// Internal die temperature sensor exposed by a board HAL.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Read the current die temperature, or `None` if the reading
    /// failed. Implementations may await on the underlying hardware
    /// conversion.
    async fn read(&mut self) -> Option<Temperature>;
}

/// Push a temperature reading as the canonical board-temperature gauge.
/// Callers that already have a value in hand (e.g. `ubos-thermal`
/// inside its sample loop) use this instead of running a second sampler.
pub async fn publish_board_temperature(temp: Temperature) {
    push(Metric::with_no_labels(
        TemperatureParams::METRIC_NAME,
        Value::Int(i64::from(temp.millicelsius())),
    ))
    .await;
}

/// Sample `sensor` at the shared period and push each reading as a
/// gauge metric. Runs forever.
pub async fn run_temperature<S: TemperatureSensor>(mut sensor: S) -> ! {
    let mut ticker = Ticker::every(TemperatureParams::PERIOD);
    loop {
        if let Some(temp) = sensor.read().await {
            publish_board_temperature(temp).await;
        }
        ticker.next().await;
    }
}
