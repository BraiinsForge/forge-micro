//! LM75A / LM75B I2C temperature sensor driver.
//!
//! Reads the 11-bit two's-complement temperature register at 0x00 and
//! converts to millicelsius. Resolution is 0.125°C on the LM75B and
//! 0.5°C on the LM75A. Both produce the same register layout, so the
//! driver handles either.
//!
//! [`Lm75aHashboard`] wraps four `Lm75a` sensors for the BMM101 layout,
//! sharing a single I2C bus and exposing the hottest reading via the
//! [`TemperatureSensor`](crate::TemperatureSensor) trait. Board firmware
//! crates instantiate this directly so the four-sensor logic does not
//! need to be duplicated per board.

use core::fmt::Write as _;

use embedded_hal_async::i2c::I2c;
use ubos_core::units::Temperature;
use ubos_logging::error;
use ubos_metrics::metrics::{push, Label, LabelValue, Labels, Metric, Value};

use crate::TemperatureSensor;

const REG_TEMPERATURE: u8 = 0x00;

/// Datasheet supported range, exclusive of garbage readings on bus
/// errors that produce e.g. `0xff80`, which decodes to a plausible
/// −1°C, or `0x7fff`. The chip itself only reports −55..=125°C.
const MIN_MILLICELSIUS: i32 = -55_000;
const MAX_MILLICELSIUS: i32 = 125_000;

/// One LM75A on the I2C bus. Address only — bus is passed in on each
/// read so multiple sensors can share a `&mut I` without owning it.
pub struct Lm75a {
    address: u8,
}

impl Lm75a {
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }

    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Read the temperature register and convert to millicelsius.
    /// Returns `Err` on bus error or out-of-range reading.
    pub async fn read_millicelsius<I, E>(&self, i2c: &mut I) -> Result<i32, Error<E>>
    where
        I: I2c<Error = E>,
    {
        let mut buffer = [0u8; 2];
        i2c.write_read(self.address, &[REG_TEMPERATURE], &mut buffer)
            .await
            .map_err(Error::I2c)?;
        let millicelsius = raw_to_millicelsius(buffer);
        if !(MIN_MILLICELSIUS..=MAX_MILLICELSIUS).contains(&millicelsius) {
            return Err(Error::OutOfRange(millicelsius));
        }
        Ok(millicelsius)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    I2c(E),
    OutOfRange(i32),
}

/// Four LM75A sensors on the BMM101 hashboard wired to one shared I2C
/// bus. Returns the hottest of the four readings each sample, offset by
/// [`Lm75aHashboard::OFFSET_MILLICELSIUS`] to approximate chip junction
/// temperature. Mirrors the `BRAIINS_MINI_BMM101_BM1362_4X1` descriptor
/// in bos-main.
pub struct Lm75aHashboard<Bus> {
    bus: Bus,
    sensors: [Lm75a; 4],
}

impl<Bus> Lm75aHashboard<Bus> {
    /// LM75A I2C addresses on the BMM101: IC12D, IC12C, IC12B, IC12A.
    pub const SENSOR_ADDRESSES: [u8; 4] = [0x49, 0x4a, 0x4b, 0x4c];

    /// Offset added to each board reading to approximate chip junction
    /// temperature. BOS uses +7 °C for every BMM101 sensor.
    pub const OFFSET_MILLICELSIUS: i32 = 7_000;

    #[must_use]
    pub fn new(bus: Bus) -> Self {
        Self {
            bus,
            sensors: Self::SENSOR_ADDRESSES.map(Lm75a::new),
        }
    }
}

impl<Bus> TemperatureSensor for Lm75aHashboard<Bus>
where
    Bus: I2c,
{
    async fn read(&mut self) -> Option<Temperature> {
        let mut hottest: Option<i32> = None;
        for sensor in &self.sensors {
            match sensor.read_millicelsius(&mut self.bus).await {
                Ok(reading) => {
                    let adjusted = reading + Self::OFFSET_MILLICELSIUS;
                    hottest = Some(hottest.map_or(adjusted, |current| current.max(adjusted)));

                    // Push the per-sensor reading labeled by I2C address so
                    // the dashboard can show hot vs cold sensors independently.
                    let mut address_str = heapless::String::<24>::new();
                    let _ = write!(address_str, "{:#04x}", sensor.address());
                    let mut labels = Labels::new();
                    let _ = labels.push(Label {
                        name: "addr",
                        value: LabelValue::Short(address_str),
                    });
                    push(Metric::with_labels(
                        "board_lm75a_temperature_millicelsius",
                        labels,
                        Value::Int(i64::from(adjusted)),
                    ))
                    .await;
                }
                Err(Error::I2c(bus_error)) => {
                    error!(
                        "temperature: LM75A {:#x} I2C error: {:?}",
                        sensor.address(),
                        ubos_logging::Debug2Format(&bus_error),
                    );
                }
                Err(Error::OutOfRange(raw)) => {
                    error!(
                        "temperature: LM75A {:#x} reading {} mC out of range",
                        sensor.address(),
                        raw,
                    );
                }
            }
        }
        hottest.map(Temperature::from_millicelsius)
    }
}

/// Decode the 16-bit big-endian register: high byte is signed integer
/// degrees C, low byte's top 3 bits are fractional in 0.125°C steps.
/// Convert directly to millicelsius without floats.
///
/// The low byte's bottom 5 bits are reserved on the LM75A/B; some
/// clones leave noise there instead of zeros. Mask them explicitly
/// before the shift so a misbehaving part cannot leak garbage into
/// the result even if the sign-extension semantics of `>>` were to
/// surprise us on a future toolchain.
fn raw_to_millicelsius(buffer: [u8; 2]) -> i32 {
    let masked = [buffer[0], buffer[1] & 0xE0];
    let raw = i16::from_be_bytes(masked) >> 5;
    i32::from(raw) * 125
}

#[cfg(test)]
#[path = "tests/lm75a.rs"]
mod tests;
