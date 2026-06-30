//! `TPS6287x` DC/DC converter register layout and bus operations.

pub(crate) const REG_VSET: u8 = 0x00;
pub(crate) const REG_CONTROL1: u8 = 0x01;
pub(crate) const REG_CONTROL2: u8 = 0x02;
pub(crate) const REG_STATUS: u8 = 0x04;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoltageRampSpeed {
    VRamp10,
    VRamp5,
    #[default]
    VRamp1_25,
    VRamp0_5,
}

impl VoltageRampSpeed {
    const fn bits(self) -> u8 {
        match self {
            VoltageRampSpeed::VRamp10 => 0b00,
            VoltageRampSpeed::VRamp5 => 0b01,
            VoltageRampSpeed::VRamp1_25 => 0b10,
            VoltageRampSpeed::VRamp0_5 => 0b11,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoftStartRampTime {
    SSTime0_5,
    #[default]
    SSTime1,
    SSTime2,
    SSTime4,
}

impl SoftStartRampTime {
    const fn bits(self) -> u8 {
        match self {
            SoftStartRampTime::SSTime0_5 => 0b00,
            SoftStartRampTime::SSTime1 => 0b01,
            SoftStartRampTime::SSTime2 => 0b10,
            SoftStartRampTime::SSTime4 => 0b11,
        }
    }

    pub(crate) const fn duration(self) -> embassy_time::Duration {
        match self {
            SoftStartRampTime::SSTime0_5 => embassy_time::Duration::from_micros(500),
            SoftStartRampTime::SSTime1 => embassy_time::Duration::from_millis(1),
            SoftStartRampTime::SSTime2 => embassy_time::Duration::from_millis(2),
            SoftStartRampTime::SSTime4 => embassy_time::Duration::from_millis(4),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoltageRange {
    /// 400–718 mV in 1.25 mV steps.
    VRange0,
    /// 400–1037 mV in 2.5 mV steps.
    VRange1,
    /// 400–1675 mV in 5 mV steps.
    #[default]
    VRange2,
    /// 800–3350 mV in 10 mV steps.
    VRange3,
}

impl VoltageRange {
    pub const fn start_mv(self) -> u32 {
        match self {
            VoltageRange::VRange0 | VoltageRange::VRange1 | VoltageRange::VRange2 => 400,
            VoltageRange::VRange3 => 800,
        }
    }

    pub const fn step_microvolts(self) -> u32 {
        match self {
            VoltageRange::VRange0 => 1_250,
            VoltageRange::VRange1 => 2_500,
            VoltageRange::VRange2 => 5_000,
            VoltageRange::VRange3 => 10_000,
        }
    }

    pub const fn end_mv(self) -> u32 {
        // 255 * step_microvolts / 1000, rounded down.
        // VRange2: 400 + 255*5 = 1675 mV.
        // VRange0: 400 + 255*1250/1000 = 400 + 318 = 718 mV.
        self.start_mv() + (255 * self.step_microvolts()) / 1_000
    }

    /// Convert millivolts to a DAC value, rounding to the nearest
    /// step. Returns `None` if `mv` is outside the supported
    /// range.
    pub fn voltage_to_dac(self, mv: u32) -> Option<u8> {
        if mv < self.start_mv() {
            return None;
        }
        let microvolts_above = (mv - self.start_mv()).checked_mul(1_000)?;
        let half_step = self.step_microvolts() / 2;
        let dac = (microvolts_above + half_step) / self.step_microvolts();
        if dac > 255 {
            return None;
        }
        Some(dac as u8)
    }

    /// Convert a DAC value to millivolts, rounded down.
    pub const fn dac_to_mv(self, dac: u8) -> u32 {
        self.start_mv() + (dac as u32 * self.step_microvolts()) / 1_000
    }

    const fn bits(self) -> u8 {
        match self {
            VoltageRange::VRange0 => 0b00,
            VoltageRange::VRange1 => 0b01,
            VoltageRange::VRange2 => 0b10,
            VoltageRange::VRange3 => 0b11,
        }
    }
}

pub(crate) fn encode_vset(dac: u8) -> [u8; 2] {
    [REG_VSET, dac]
}

/// CONTROL1 with `dischen=1` (output discharge enabled to match
/// BOS default), optional `swen`, and the configured ramp speed.
pub(crate) fn encode_control1(swen: bool, vramp: VoltageRampSpeed) -> [u8; 2] {
    let mut value = 0u8;
    if swen {
        value |= 1 << 5;
    }
    value |= 1 << 3; // dischen
    value |= vramp.bits();
    [REG_CONTROL1, value]
}

/// CONTROL1 reset pulse — bit 7 set, all other fields zero.
/// Hardware clears the bit after self-reset completes.
pub(crate) fn encode_control1_reset() -> [u8; 2] {
    [REG_CONTROL1, 1 << 7]
}

pub(crate) fn encode_control2(vrange: VoltageRange, sstime: SoftStartRampTime) -> [u8; 2] {
    let value = (vrange.bits() << 2) | sstime.bits();
    [REG_CONTROL2, value]
}

pub(crate) fn decode_status(byte: u8) -> crate::StatusFlags {
    crate::StatusFlags {
        undervoltage: (byte & 0b0000_0010) != 0,
        overvoltage: (byte & 0b0000_0001) != 0,
        thermal_shutdown: (byte & 0b0000_0100) != 0,
        current_limit: (byte & 0b0001_0000) != 0,
    }
}

use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

use crate::{BmmPsuConfig, Error};

/// `t_d(EN)2` = 100 µs after the reset pulse before further writes.
const SW_ENABLE_DELAY: Duration = Duration::from_micros(100);

pub(crate) async fn init<I, E>(i2c: &mut I, config: &BmmPsuConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    reset(i2c, config).await?;
    i2c.write(
        config.dcdc_address,
        &encode_control2(config.dcdc_voltage_range, config.dcdc_soft_start),
    )
    .await
    .map_err(Error::Bus)?;
    let dac = config
        .dcdc_voltage_range
        .voltage_to_dac(config.dcdc_initial.mv())
        .ok_or(Error::OutOfRange)?;
    i2c.write(config.dcdc_address, &encode_vset(dac))
        .await
        .map_err(Error::Bus)
}

async fn reset<I, E>(i2c: &mut I, config: &BmmPsuConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(config.dcdc_address, &encode_control1_reset())
        .await
        .map_err(Error::Bus)?;
    Timer::after(SW_ENABLE_DELAY).await;
    i2c.write(
        config.dcdc_address,
        &encode_control1(false, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)
}

pub(crate) async fn enable<I, E>(i2c: &mut I, config: &BmmPsuConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(
        config.dcdc_address,
        &encode_control1(true, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)?;
    Timer::after(config.dcdc_soft_start.duration()).await;
    // Read STATUS to clear bits latched during the soft-start
    // ramp (undervoltage is transient until the rail settles).
    // Real faults surface on the next user call; matches BOS.
    let _ = read_status(i2c, config).await;
    Ok(())
}

pub(crate) async fn disable<I, E>(i2c: &mut I, config: &BmmPsuConfig) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    i2c.write(
        config.dcdc_address,
        &encode_control1(false, config.dcdc_ramp_speed),
    )
    .await
    .map_err(Error::Bus)
}

pub(crate) async fn set_voltage<I, E>(
    i2c: &mut I,
    config: &BmmPsuConfig,
    millivolts: u32,
) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let dac = config
        .dcdc_voltage_range
        .voltage_to_dac(millivolts)
        .ok_or(Error::OutOfRange)?;
    i2c.write(config.dcdc_address, &encode_vset(dac))
        .await
        .map_err(Error::Bus)
}

pub(crate) async fn read_status<I, E>(
    i2c: &mut I,
    config: &BmmPsuConfig,
) -> Result<crate::StatusFlags, Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
{
    let mut buffer = [0u8; 1];
    i2c.write_read(config.dcdc_address, &[REG_STATUS], &mut buffer)
        .await
        .map_err(Error::Bus)?;
    Ok(decode_status(buffer[0]))
}

#[cfg(test)]
#[path = "tests/tps6287x.rs"]
mod tests;
