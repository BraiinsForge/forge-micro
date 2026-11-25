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

//! The value of one metric: a typed unit, and the text a scrape shows
//! for it.
//!
//! The store holds the unit a reading was measured in, so the series and
//! its number cannot disagree, and [`Value`]'s [`Display`](fmt::Display)
//! is the one place that turns a unit into exposition text. Every variant
//! writes its base unit with integer math, because a float render costs
//! several times as much on a board with no double-precision unit.

use core::fmt;
use core::num::NonZeroU64;

use ubos_core::units::{Current, FanPwm, FanRpm, Frequency, Power, Temperature, Voltage};

#[cfg(test)]
#[path = "tests/value.rs"]
mod tests;

/// Hertz in one kilohertz, the scale between the unit a [`Frequency`]
/// holds and the base unit a scrape shows.
const HERTZ_PER_KILOHERTZ: u64 = 1000;

/// Thousandths in one whole, the scale a [`Ratio`] is held in.
const THOUSANDTHS_PER_WHOLE: u16 = 1000;

/// A share of a whole between 0 and 1, held in thousandths.
///
/// It is the one unit this crate owns, because only a metric states a
/// ratio. The value is an integer so that neither building nor writing
/// one needs floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio(u16);

impl Ratio {
    /// The share `part` is of `whole`, rounded to the nearest thousandth.
    /// A zero `whole` gives 0 and a `part` above `whole` gives 1, so a
    /// window with no elapsed time and a counter that ran ahead both
    /// stay in range.
    #[must_use]
    pub fn new(part: u64, whole: u64) -> Self {
        let Some(whole) = NonZeroU64::new(whole) else {
            return Self(0);
        };
        let whole = u128::from(whole.get());
        let part = u128::from(part).min(whole);
        // The product is below 2^74 and the quotient is at most a thousand.
        let thousandths = (part * u128::from(THOUSANDTHS_PER_WHOLE) + whole / 2) / whole;
        Self(u16::try_from(thousandths).unwrap_or(THOUSANDTHS_PER_WHOLE))
    }
}

/// The ratio with three decimals, such as `0.250`.
impl fmt::Display for Ratio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:03}",
            self.0 / THOUSANDTHS_PER_WHOLE,
            self.0 % THOUSANDTHS_PER_WHOLE
        )
    }
}

/// A metric value: the unit a reading was taken in, a flag, or a count.
///
/// The set is closed, so a reader of the store knows every quantity a
/// series can hold. A push site passes the unit it holds, and a flag or
/// a count is named at the site, because a bare integer has no meaning
/// of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// A voltage, shown in volts.
    Voltage(Voltage),
    /// A temperature, shown in degrees Celsius.
    Temperature(Temperature),
    /// A power, shown in watts.
    Power(Power),
    /// A current, shown in amps.
    Current(Current),
    /// A clock frequency, shown in hertz.
    Frequency(Frequency),
    /// A fan duty cycle, shown in percent.
    FanPwm(FanPwm),
    /// A fan speed, shown in revolutions per minute.
    FanRpm(FanRpm),
    /// A share of a whole, shown between 0 and 1.
    Ratio(Ratio),
    /// A condition that holds or does not, shown as 1 or 0.
    Flag(bool),
    /// A number with no unit, such as a counter.
    Count(u64),
}

/// The value in its base unit, as one exposition line carries it.
///
/// A unit held in thousandths keeps its three decimals, trailing zeros
/// included, so 21 600 mV reads `21.600`.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Voltage(voltage) => voltage.fmt(f),
            Self::Temperature(temperature) => temperature.fmt(f),
            Self::Power(power) => power.fmt(f),
            Self::Current(current) => current.fmt(f),
            // A `u32` of kilohertz times a thousand is below 2^42.
            Self::Frequency(frequency) => (u64::from(frequency.khz()) * HERTZ_PER_KILOHERTZ).fmt(f),
            Self::FanPwm(pwm) => pwm.percent().fmt(f),
            Self::FanRpm(rpm) => rpm.rpm().fmt(f),
            Self::Ratio(ratio) => ratio.fmt(f),
            Self::Flag(flag) => u8::from(*flag).fmt(f),
            Self::Count(count) => count.fmt(f),
        }
    }
}

/// Implements `From` for each unit whose variant carries its name, so a
/// push site hands over the unit it holds.
macro_rules! value_from_unit {
    ($($unit:ident),+ $(,)?) => {
        $(
            impl From<$unit> for Value {
                fn from(unit: $unit) -> Self {
                    Self::$unit(unit)
                }
            }
        )+
    };
}

value_from_unit!(
    Voltage,
    Temperature,
    Power,
    Current,
    Frequency,
    FanPwm,
    FanRpm,
    Ratio,
);

/// A number with no unit, such as a counter.
///
/// A push site wraps its integer in it, so the site states that the
/// number counts something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count(pub u64);

/// A condition that holds or does not.
///
/// A push site wraps its `bool` in it, so the site states that the
/// series reads 1 or 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flag(pub bool);

impl From<Count> for Value {
    fn from(count: Count) -> Self {
        Self::Count(count.0)
    }
}

impl From<Flag> for Value {
    fn from(flag: Flag) -> Self {
        Self::Flag(flag.0)
    }
}
