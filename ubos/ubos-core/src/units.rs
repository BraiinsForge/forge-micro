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

//! Typed units for the seven quantities that cross module boundaries in
//! the miner: frequency, voltage, power, efficiency, temperature, fan
//! PWM, and fan RPM. Each newtype wraps the canonical scalar already
//! used in the codebase (kHz, mV, mW, mJ/TH, millicelsius, percent,
//! RPM) so the refactor is mechanical: bare integers at signatures
//! become typed values, conversions stay inside the driver that owns
//! the wire encoding. The newtypes are `#[repr(transparent)]` over
//! their inner scalar, so atomic storage and FFI layouts are unaffected.

use core::num::NonZeroU32;

/// Clock frequency in kilohertz. The internal scalar is kHz so a
/// 6.25 MHz PLL step survives the type without truncation; consumers
/// that operate at MHz granularity (config, dashboard, telemetry)
/// use [`Self::mhz`], and the chip-programming path uses
/// [`Self::khz`] to keep the PLL calculator exact.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Frequency(u32);

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

    /// Convert to megahertz as `f32`. The tuner's V^2*f curve walk
    /// needs fractional MHz to compute the next step's voltage from
    /// the constant-power invariant; the integer `mhz()` rounds.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_mhz(self) -> f32 {
        self.0 as f32 / 1000.0
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
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Voltage(u32);

impl Voltage {
    #[must_use]
    pub const fn from_mv(mv: u32) -> Self {
        Self(mv)
    }

    #[must_use]
    pub const fn mv(self) -> u32 {
        self.0
    }

    /// Convert to volts as `f32`. The tuner's V^2*f curve walk needs
    /// fractional volts to compute the next voltage step from the
    /// constant-power invariant; integer millivolts cannot represent
    /// the square-root output without truncation.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_volts(self) -> f32 {
        self.0 as f32 / 1000.0
    }
}

/// Electrical power in milliwatts. PSU-side sensors report this value,
/// and measured-power control loops use it as a target and feedback.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Power(u32);

impl Power {
    #[must_use]
    pub const fn from_mw(mw: u32) -> Self {
        Self(mw)
    }

    #[must_use]
    pub const fn mw(self) -> u32 {
        self.0
    }
}

/// Energy per hash in millijoules per terahash, against the theoretical
/// hashrate of one hash per core per clock cycle.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Efficiency(NonZeroU32);

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

/// Temperature in millicelsius. The LM75A driver decodes its register
/// directly into this unit, and the metrics layer publishes it
/// unchanged. The PID loop in `ubos-cooling` consumes
/// [`celsius_f32`](Self::celsius_f32) to operate in floating-point
/// degrees Celsius; that is the only sanctioned lossy conversion.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Temperature(i32);

impl Temperature {
    #[must_use]
    pub const fn from_millicelsius(millicelsius: i32) -> Self {
        Self(millicelsius)
    }

    #[must_use]
    pub const fn millicelsius(self) -> i32 {
        self.0
    }

    /// Convert to degrees Celsius as `f32`. Lossy: callers that need
    /// fractional degrees use this accessor, including the cooling PID
    /// loop, thermal threshold evaluation, and dashboard reporting.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn celsius_f32(self) -> f32 {
        self.0 as f32 / 1000.0
    }
}

/// Fan PWM duty cycle in percent, clamped to `0..=100`. The
/// constructor saturates rather than failing because every existing
/// producer (PID output, configuration parser, dashboard slider) is
/// expected to be in range and the clamp gives a safer fallback than
/// an `Option` that callers would unwrap.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[cfg(test)]
#[path = "tests/units.rs"]
mod tests;
