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

//! Typed units for the eight quantities that cross module boundaries in
//! the miner: frequency, voltage, power, current, efficiency,
//! temperature, fan PWM, and fan RPM. Each newtype wraps the canonical
//! scalar already used in the codebase (kHz, mV, mW, mA, mJ/TH,
//! millicelsius, percent, RPM) so the refactor is mechanical: bare
//! integers at signatures become typed values, conversions stay inside
//! the driver that owns the wire encoding. The newtypes are `#[repr(transparent)]` over
//! their inner scalar, so atomic storage and FFI layouts are unaffected.

use core::fmt;
use core::num::NonZeroU32;

#[cfg(test)]
#[path = "tests/units.rs"]
mod tests;

/// Clock frequency in kilohertz. The internal scalar is kHz so a
/// 6.25 MHz PLL step survives the type without truncation; consumers
/// that operate at MHz granularity (config, dashboard, telemetry)
/// use [`Self::mhz`], and the chip-programming path uses
/// [`Self::khz`] to keep the PLL calculator exact.
#[repr(transparent)]
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Frequency(u32);

/// The schema of the integer a [`Frequency`] serializes as, so a record that
/// holds one keeps the schema hash it had when it held the integer.
impl postcard_schema::Schema for Frequency {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        <u32 as postcard_schema::Schema>::SCHEMA;
}

/// The clock in kHz at which one core, one hash per cycle, hashes 1 TH/s.
const CORE_KHZ_PER_TERAHASH: u64 = 1_000_000_000;

impl Frequency {
    #[must_use]
    pub const fn from_mhz(mhz: u32) -> Self {
        Self(mhz.saturating_mul(1000))
    }

    #[must_use]
    pub const fn from_khz(khz: u32) -> Self {
        Self(khz)
    }

    /// The clock at which `cores` cores, one hash per core per cycle, draw
    /// `power` at `efficiency`. `None` when `cores` is zero, or when the
    /// clock does not fit a `u32` in kHz.
    #[must_use]
    pub fn try_from_power_and_efficiency(
        power: Power,
        efficiency: Efficiency,
        cores: u64,
    ) -> Option<Self> {
        // `mW / (mJ/TH)` is the hashrate in TH/s, shared by every core.
        let khz = (u64::from(power.mw()) * CORE_KHZ_PER_TERAHASH)
            .checked_div(u64::from(efficiency.mj_per_th()).saturating_mul(cores))?;
        u32::try_from(khz).ok().map(Self::from_khz)
    }

    #[must_use]
    pub const fn mhz(self) -> u32 {
        self.0 / 1000
    }

    #[must_use]
    pub const fn khz(self) -> u32 {
        self.0
    }

    /// Round down to a multiple of `step` so the frequency fits a discrete clock grid.
    ///
    /// # Panics
    ///
    /// Panics when `step` is zero.
    #[must_use]
    pub fn snap_down(self, step: Self) -> Self {
        assert!(step.0 != 0, "BUG: frequency step must be nonzero");
        Self(self.0 / step.0 * step.0)
    }

    /// Move toward `target` by at most `step`, without crossing the target.
    ///
    /// The comparison runs in integer kHz, so the walk converges even
    /// when `target` came from float rounding inside the V^2 * f math.
    /// A float comparison would tail-loop on the last fraction of a step.
    ///
    /// # Panics
    ///
    /// Panics when `step` is zero.
    #[must_use]
    pub fn step_toward(self, target: Self, step: Self) -> Self {
        use core::cmp::Ordering;

        assert!(step.0 != 0, "BUG: frequency step must be nonzero");
        match self.cmp(&target) {
            Ordering::Less => Self(self.0.saturating_add(step.0).min(target.0)),
            Ordering::Greater => Self(self.0.saturating_sub(step.0).max(target.0)),
            Ordering::Equal => self,
        }
    }

    /// Convert to megahertz as `f64`, for a caller that needs the
    /// fraction of a megahertz that the integer `mhz()` drops.
    #[must_use]
    pub fn as_mhz(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    /// Construct from fractional megahertz, for a model that solves in
    /// them, rounded to the nearest kilohertz. A negative or not-a-number
    /// value gives zero, and one past the range gives the largest value.
    // A float to integer cast saturates, which is the behaviour stated above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[must_use]
    pub fn from_mhz_f64(mhz: f64) -> Self {
        Self((mhz * 1000.0 + 0.5) as u32)
    }
}

/// Inclusive operating-frequency envelope. A tuner ramps inside this
/// range and register calculators reject targets outside it.
#[derive(Clone, Copy)]
pub struct FrequencyRange {
    min: Frequency,
    max: Frequency,
}

impl FrequencyRange {
    /// Constructs the range from an inclusive lower and upper bound.
    #[must_use]
    pub const fn new(min: Frequency, max: Frequency) -> Self {
        Self { min, max }
    }

    /// Inclusive lower bound of the operating-frequency range.
    #[must_use]
    pub const fn min(self) -> Frequency {
        self.min
    }

    /// Inclusive upper bound of the operating-frequency range.
    #[must_use]
    pub const fn max(self) -> Frequency {
        self.max
    }
}

/// Voltage in millivolts. Used for ASIC core voltage targets and PSU
/// DAC encoding. Negative values are not representable; the rails the
/// miner controls are all single-ended positive supplies.
#[repr(transparent)]
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Voltage(u32);

/// The schema of the integer a [`Voltage`] serializes as, so a record that
/// holds one keeps the schema hash it had when it held the integer.
impl postcard_schema::Schema for Voltage {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        <u32 as postcard_schema::Schema>::SCHEMA;
}

impl Voltage {
    #[must_use]
    pub const fn from_mv(mv: u32) -> Self {
        Self(mv)
    }

    #[must_use]
    pub const fn mv(self) -> u32 {
        self.0
    }

    /// Convert to volts as `f64`, for a caller that computes in volts
    /// rather than in integer millivolts.
    #[must_use]
    pub fn as_volts(self) -> f64 {
        f64::from(self.0) / 1000.0
    }
}

/// Electrical power in milliwatts. PSU-side sensors report this value,
/// and measured-power control loops use it as a target and feedback.
#[repr(transparent)]
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Power(u32);

/// The schema of the integer a [`Power`] serializes as, so a record that
/// holds one keeps the schema hash it had when it held the integer.
impl postcard_schema::Schema for Power {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        <u32 as postcard_schema::Schema>::SCHEMA;
}

impl Power {
    #[must_use]
    pub const fn from_mw(mw: u32) -> Self {
        Self(mw)
    }

    #[must_use]
    pub const fn mw(self) -> u32 {
        self.0
    }

    /// Convert to watts as `f64`, for a caller that computes in watts
    /// rather than in integer milliwatts.
    #[must_use]
    pub fn as_watts(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    /// Construct from fractional watts, for a model that computes in
    /// them, rounded to the nearest milliwatt. A negative or not-a-number
    /// value gives zero, and one past the range gives the largest value.
    // A float to integer cast saturates, which is the behaviour stated above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[must_use]
    pub fn from_watts_f64(watts: f64) -> Self {
        Self((watts * 1000.0 + 0.5) as u32)
    }
}

/// Electrical current in milliamps. PSU and shunt sensors report it, and
/// a driver programs its limits with it.
#[repr(transparent)]
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Current(u32);

/// The schema of the integer a [`Current`] serializes as, so a record that
/// holds one keeps the schema hash it had when it held the integer.
impl postcard_schema::Schema for Current {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        <u32 as postcard_schema::Schema>::SCHEMA;
}

impl Current {
    #[must_use]
    pub const fn from_ma(ma: u32) -> Self {
        Self(ma)
    }

    /// Construct from whole amps, for a value stated in amps, such as a
    /// supply's rated limit.
    #[must_use]
    pub const fn from_amps(amps: u32) -> Self {
        Self(amps.saturating_mul(1000))
    }

    #[must_use]
    pub const fn ma(self) -> u32 {
        self.0
    }
}

/// Energy per hash in millijoules per terahash, against the theoretical
/// hashrate of one hash per core per clock cycle.
///
/// It serializes as its integer mJ/TH, and a stored zero does not
/// deserialize.
#[repr(transparent)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Efficiency(NonZeroU32);

/// The schema of the integer an [`Efficiency`] serializes as, so a record
/// that holds one keeps the schema hash it had when it held the integer.
impl postcard_schema::Schema for Efficiency {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        <NonZeroU32 as postcard_schema::Schema>::SCHEMA;
}

impl Efficiency {
    #[must_use]
    pub const fn from_mj_per_th(mj_per_th: NonZeroU32) -> Self {
        Self(mj_per_th)
    }

    #[must_use]
    pub const fn mj_per_th(self) -> u32 {
        self.0.get()
    }
}

/// Writes a unit that is held in thousandths of the unit it is shown in,
/// as those units with three decimals, for the log line and the event
/// log. The division runs in integers, so a log line pulls in no software
/// floating point.
macro_rules! show_in_thousandths {
    ($unit:ty, |$value:ident| $thousandths:expr) => {
        impl fmt::Display for $unit {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let $value = self;
                let thousandths: u32 = $thousandths;
                write!(f, "{}.{:03}", thousandths / 1000, thousandths % 1000)
            }
        }

        #[cfg(feature = "defmt")]
        impl defmt::Format for $unit {
            fn format(&self, f: defmt::Formatter<'_>) {
                let $value = self;
                let thousandths: u32 = $thousandths;
                defmt::write!(
                    f,
                    "{=u32}.{=u32:03}",
                    thousandths / 1000,
                    thousandths % 1000
                );
            }
        }
    };
}

// Megahertz, volts, watts, and joules per terahash.
show_in_thousandths!(Frequency, |frequency| frequency.khz());
show_in_thousandths!(Voltage, |voltage| voltage.mv());
show_in_thousandths!(Power, |power| power.mw());
show_in_thousandths!(Efficiency, |efficiency| efficiency.mj_per_th());

/// Temperature in millicelsius, and the one type that firmware holds and
/// compares a temperature in. A raw number appears only where a value
/// leaves the firmware: [`millicelsius`](Self::millicelsius) for the
/// metrics, [`celsius_i32`](Self::celsius_i32) for the API, and
/// [`celsius_f64`](Self::celsius_f64) for the PID loop in `ubos-cooling`,
/// which operates in the unit its gains are tuned in.
///
/// It serializes as its integer millicelsius, so a config that holds it
/// stores and exchanges the same number the type holds.
#[repr(transparent)]
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[serde(transparent)]
pub struct Temperature(i32);

/// Degrees Celsius with three decimals, computed in integers so that a log
/// line pulls in no software floating point.
impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let magnitude = self.0.unsigned_abs();
        write!(f, "{sign}{}.{:03}", magnitude / 1000, magnitude % 1000)
    }
}

/// Degrees Celsius, as [`Display`](fmt::Display) writes them.
#[cfg(feature = "defmt")]
impl defmt::Format for Temperature {
    fn format(&self, f: defmt::Formatter<'_>) {
        let sign = if self.0 < 0 { "-" } else { "" };
        let magnitude = self.0.unsigned_abs();
        defmt::write!(
            f,
            "{=str}{=u32}.{=u32:03}",
            sign,
            magnitude / 1000,
            magnitude % 1000
        );
    }
}

impl Temperature {
    #[must_use]
    pub const fn from_millicelsius(millicelsius: i32) -> Self {
        Self(millicelsius)
    }

    /// Construct from fractional degrees Celsius, for a driver whose
    /// conversion formula computes in them, rounded to the nearest
    /// millicelsius.
    // A temperature in degrees times a thousand is well inside i32.
    #[allow(clippy::cast_possible_truncation)]
    #[must_use]
    pub fn from_celsius_f64(celsius: f64) -> Self {
        let millicelsius = celsius * 1000.0;
        let nudge = if millicelsius < 0.0 { -0.5 } else { 0.5 };
        Self((millicelsius + nudge) as i32)
    }

    /// Construct from whole degrees Celsius, for a value stated in degrees,
    /// such as a board's setpoint or halt threshold.
    #[must_use]
    pub const fn from_celsius(celsius: i32) -> Self {
        Self(celsius.saturating_mul(1000))
    }

    #[must_use]
    pub const fn millicelsius(self) -> i32 {
        self.0
    }

    /// Whole degrees Celsius, truncated toward zero, for a caller that
    /// reports a temperature in whole degrees.
    #[must_use]
    pub const fn celsius_i32(self) -> i32 {
        self.0 / 1000
    }

    /// Convert to degrees Celsius as `f64`, for a caller that computes in
    /// fractional degrees, such as the cooling PID loop.
    #[must_use]
    pub fn celsius_f64(self) -> f64 {
        f64::from(self.0) / 1000.0
    }
}

/// Fan PWM duty cycle in percent, clamped to `0..=100`. The
/// constructor saturates rather than failing because every existing
/// producer (PID output, configuration parser, dashboard slider) is
/// expected to be in range and the clamp gives a safer fallback than
/// an `Option` that callers would unwrap.
///
/// It serializes as its integer percent. It has no `Deserialize`, so a
/// value read from outside cannot skip the clamp.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct FanPwm(u8);

impl FanPwm {
    pub const ZERO: Self = Self(0);
    pub const FULL: Self = Self(100);

    #[must_use]
    pub const fn from_percent(percent: u8) -> Self {
        let clamped = if percent > 100 { 100 } else { percent };
        Self(clamped)
    }

    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }
}

/// Fan tachometer reading in revolutions per minute. Read-only
/// telemetry: the tach task derives it from pulse counts and
/// publishes it, no control loop consumes it.
///
/// It serializes as its integer RPM.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct FanRpm(u32);

impl FanRpm {
    #[must_use]
    pub const fn from_rpm(rpm: u32) -> Self {
        Self(rpm)
    }

    #[must_use]
    pub const fn rpm(self) -> u32 {
        self.0
    }
}
