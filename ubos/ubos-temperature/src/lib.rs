//! Board-temperature sensor trait, periodic sampler task, and shared
//! constants for the Prometheus temperature gauge. Hardware drivers
//! (LM75A, etc.) live in sibling modules of this crate; board firmware
//! crates supply their own die-sensor `TemperatureSensor` impl.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod hashnode;
pub mod lm75a;
pub mod tmp102;
pub mod tmp461;

pub use hashnode::HashnodeTemperature;

use embassy_time::{Duration, Ticker};
use ubos_core::units::Temperature;
use ubos_metrics::metrics::{Label, LabelValue, Labels, Metric, Value, push};

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

/// Source label marking a safety reading as the real die temperature rather
/// than a PCB-derived approximation. The thermal layer keys the die-specific
/// danger threshold off this exact label, so producer and consumer share it.
pub const CHIP_SOURCE: &str = "chip";

/// Internal die temperature sensor exposed by a board HAL.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Read the current die temperature, or `None` if the reading
    /// failed. This is the value that gates thermal shutdown. Implementations
    /// may await on the underlying hardware conversion.
    async fn read(&mut self) -> Option<Temperature>;

    /// Temperature that should drive the cooling PID when it differs from
    /// the safety reading returned by [`read`](Self::read), cached from the
    /// most recent `read`. `None` means the PID uses the safety reading, so
    /// single-sensor boards need not implement it.
    fn pid_temperature(&self) -> Option<Temperature> {
        None
    }

    /// Source label for the safety reading, or `None` to publish the
    /// canonical gauge without a `source` label. Boards that report more
    /// than one chip-temperature estimate distinguish them this way.
    fn temperature_source(&self) -> Option<&'static str> {
        None
    }

    /// Source label for [`pid_temperature`](Self::pid_temperature).
    fn pid_temperature_source(&self) -> Option<&'static str> {
        None
    }
}

/// Push a temperature reading as the canonical board-temperature gauge.
/// Callers that already have a value in hand (e.g. `ubos-thermal`
/// inside its sample loop) use this instead of running a second sampler.
pub async fn publish_board_temperature(temp: Temperature) {
    publish_board_temperature_with_source(temp, None).await;
}

/// Push a board-temperature reading, optionally tagged with a `source`
/// label so a board that reports more than one chip-temperature estimate
/// (real die versus approximated) keeps them as distinct series under one
/// metric name.
pub async fn publish_board_temperature_with_source(
    temp: Temperature,
    source: Option<&'static str>,
) {
    let value = Value::Int(i64::from(temp.millicelsius()));
    let Some(source) = source else {
        push(Metric::with_no_labels(
            TemperatureParams::METRIC_NAME,
            value,
        ))
        .await;
        return;
    };
    let mut labels = Labels::new();
    let _ = labels.push(Label {
        name: "source",
        value: LabelValue::Static(source),
    });
    push(Metric::with_labels(
        TemperatureParams::METRIC_NAME,
        labels,
        value,
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
