//! `SLG46826G` heartbeat-driven watchdog circuit.

use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

const REG_VIRTUAL_INPUT: u8 = 0x7a;
const HEARTBEAT_HIGH_DURATION: Duration = Duration::from_millis(100);

pub(crate) async fn init<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}

pub(crate) async fn heartbeat<I, E>(i2c: &mut I, address: u8) -> Result<(), E>
where
    I: I2c<Error = E>,
{
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x01]).await?;
    Timer::after(HEARTBEAT_HIGH_DURATION).await;
    i2c.write(address, &[REG_VIRTUAL_INPUT, 0x00]).await
}

#[cfg(test)]
#[path = "tests/watchdog.rs"]
mod tests;
