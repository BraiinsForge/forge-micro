//! Periodic internal chip temperature sampler. Each board provides a
//! [`TemperatureSensor`] implementation over its HAL and passes it to
//! [`run_temperature`], which reads once per [`Temperature::PERIOD`]
//! and pushes a [`Temperature::METRIC_NAME`] gauge into the shared
//! metrics channel. Readings come from untrimmed die sensors; absolute
//! accuracy varies by chip family and is fine for drift monitoring
//! but not for thermal management. A failed read skips the push for
//! that tick, so the exporter keeps serving the previous value until
//! the next successful sample.

use embassy_time::Ticker;

use crate::consts::Temperature;
use crate::metrics::{push, Metric, Value};

/// Internal die temperature sensor exposed by a board HAL.
#[allow(async_fn_in_trait)]
pub trait TemperatureSensor {
    /// Read the current die temperature in millidegrees Celsius, or
    /// `None` if the reading failed. Implementations may await on the
    /// underlying hardware conversion. Millidegrees keep sub-degree
    /// resolution without pulling software floating-point into the
    /// periodic sampling path.
    async fn read_millicelsius(&mut self) -> Option<i32>;
}

/// Sample `sensor` at the shared period and push each reading as a
/// gauge metric. Runs forever.
pub async fn run_temperature<S: TemperatureSensor>(mut sensor: S) -> ! {
    let mut ticker = Ticker::every(Temperature::PERIOD);
    loop {
        if let Some(millicelsius) = sensor.read_millicelsius().await {
            push(Metric {
                name: Temperature::METRIC_NAME,
                labels: heapless::Vec::new(),
                value: Value::Int(i64::from(millicelsius)),
                timestamp_ms: 0,
            })
            .await;
        }
        ticker.next().await;
    }
}
