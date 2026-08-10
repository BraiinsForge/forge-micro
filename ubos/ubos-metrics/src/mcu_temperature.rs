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

//! MCU internal temperature gauge.
//!
//! A board whose MCU carries an on-die thermometer samples it here. The
//! reading measures the control board and nothing else, so it is a
//! separate series from the hashboard temperature that `ubos-thermal`
//! publishes, and no control loop reads it.
//!
//! Metrics: `../../../docs/METRICS.md`.

use embassy_time::{Duration, Ticker};
use ubos_core::units::Temperature;

use crate::metrics::{Metric, Value, push};

/// Sample period. The MCU die temperature moves slowly and only a
/// scraper reads it, so one sample per second is enough.
const PERIOD: Duration = Duration::from_secs(1);

/// Gauge name. Integer millicelsius rather than float celsius, for the
/// reason in `docs/METRICS.md`.
const METRIC_NAME: &str = "board_mcu_temperature_millicelsius";

/// The MCU internal temperature sensor exposed by a board HAL.
#[allow(async_fn_in_trait)]
pub trait McuTemperatureSensor {
    /// Read the current MCU die temperature, or `None` if the reading
    /// failed. Implementations may await on the underlying hardware
    /// conversion.
    async fn read(&mut self) -> Option<Temperature>;
}

/// Sample `sensor` at [`PERIOD`] and publish each reading as the MCU
/// temperature gauge. Runs forever.
pub async fn run_mcu_temperature<S: McuTemperatureSensor>(mut sensor: S) -> ! {
    let mut ticker = Ticker::every(PERIOD);
    loop {
        if let Some(temp) = sensor.read().await {
            push(Metric::with_no_labels(
                METRIC_NAME,
                Value::Int(i64::from(temp.millicelsius())),
            ))
            .await;
        }
        ticker.next().await;
    }
}

/// Task shell around [`run_mcu_temperature`]. Takes the board's sensor
/// type, which reaches the MCU's on-die thermometer through
/// [`McuTemperatureSensor`]. The hashboard sensors are
/// `ubos_thermal::thermal_task`'s.
#[macro_export]
macro_rules! mcu_temperature_task {
    ($sensor:ty) => {
        #[embassy_executor::task]
        async fn mcu_temperature_task(sensor: $sensor) -> ! {
            ubos_metrics::mcu_temperature::run_mcu_temperature(sensor).await
        }
    };
}
