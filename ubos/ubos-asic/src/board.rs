//! Hashboard descriptors.
//!
//! A [`BoardDescriptor`] carries the chip model and per-board topology
//! (series voltage domains, chip count, PSU ceiling) so the tuner can
//! dispatch the bring-up recipe and read chip capabilities through a
//! board-scoped seam. The static instances (such as [`BMM100`]) name the
//! concrete boards the firmware supports.

use ubos_core::units::Voltage;

use crate::chip::{Chip, ChipProtocol};

#[cfg(test)]
#[path = "tests/board.rs"]
mod tests;

/// Per-board cooling profile. Grouped onto [`BoardDescriptor`] so the
/// cooling defaults travel with the board rather than as loose fields.
/// The hashnode runs die-temp blind (a synthetic pcb+offset proxy), so it
/// targets a lower setpoint and pins a higher fan floor than the default
/// board.
#[derive(Clone, Copy)]
pub struct BoardCooling {
    /// Cooling PID setpoint in Celsius.
    pub target_temp_c: f32,
    /// Cooling PID fan-floor (minimum duty).
    pub min_fan_pwm: u8,
}

/// Hashboard-level descriptor. Carries the chip model so the tuner
/// can dispatch the bring-up recipe and read chip capabilities through
/// a board-scoped seam; per-board topology fields land here alongside
/// the code that consumes them.
#[derive(Clone, Copy)]
pub struct BoardDescriptor {
    /// Chip model populated on this hashboard. Selects the bring-up
    /// recipe and capability bounds the tuner drives.
    pub chip: Chip,
    /// Number of voltage domains stacked in series on the rail. The
    /// PSU programs the sum across the stack, so the rail-side
    /// ceiling is `chip.max_chip_voltage * num_voltage_domains`.
    pub num_voltage_domains: u8,
    /// Number of chips on the serial chain. Drives the per-chip
    /// bring-up loops and the nonce-space division. Equals
    /// `num_voltage_domains` only when each chip is a single domain.
    pub num_chips: u8,
    /// Upper bound the PSU enforces on this board's rail. The
    /// `max_rail_voltage` accessor clamps the chip-stack product
    /// against this so `Psu::set_voltage` never returns
    /// `Error::OutOfRange` from a descriptor / PSU range mismatch.
    /// Matches the `end_mv()` of the `VoltageRange` the firmware
    /// configures on the board's PSU.
    // TODO: PSU-specific fields are currently bundled into the board
    // descriptor, which is acceptable for now, but should be split into
    // a dedicated PSU descriptor later.
    pub max_psu_voltage: Voltage,
    /// Operator tuning-voltage policy (the rail span the UI/tuner may
    /// request). On the series-stacked BM1370 board this is the full-rail
    /// span, not a per-chip figure.
    pub tuning_voltage_min_mv: u32,
    pub tuning_voltage_max_mv: u32,
    /// Boot operating point, so a board comes up at its correct rail and
    /// clock from its descriptor alone, with no `.cargo/config.toml` env.
    pub default_voltage_mv: u32,
    pub default_frequency_mhz: u32,
    /// Per-board cooling profile (PID setpoint and fan floor).
    pub cooling: BoardCooling,
    /// Product name reported to external clients and the dashboard, sited
    /// here so the active board's name has one home.
    pub model: &'static str,
}

impl BoardDescriptor {
    /// Open-core rail voltage: the per-chip maximum scaled by the
    /// number of series-stacked voltage domains, clamped against the
    /// PSU's configured upper bound. The tuner anchors the
    /// `V^2 * f` curve here before any clock is applied, and the
    /// rail walks down V as f climbs. Not `const`: it reads
    /// `Chip::descriptor`, a non-const trait method.
    #[must_use]
    pub fn max_rail_voltage(&self) -> Voltage {
        let chip_stack = self
            .chip
            .descriptor()
            .max_chip_voltage
            .mv()
            .saturating_mul(u32::from(self.num_voltage_domains));
        let psu_max = self.max_psu_voltage.mv();
        Voltage::from_mv(chip_stack.min(psu_max))
    }
}

/// Hashboard descriptor. The `max_psu_voltage` mirrors
/// `MiniminerPsuConfig::DEFAULT.dcdc_voltage_range` (`VRange2`, end
/// 1675 mV).
pub const BMM100: BoardDescriptor = BoardDescriptor {
    chip: Chip::Bm1362,
    num_voltage_domains: 4,
    num_chips: 4,
    max_psu_voltage: Voltage::from_mv(1675),
    // Per-chip tuning policy, kept below the 1675 mV PSU ceiling.
    tuning_voltage_min_mv: 1100,
    tuning_voltage_max_mv: 1500,
    default_voltage_mv: ubos_core::consts::Mining::DEFAULT_VOLTAGE.mv(),
    default_frequency_mhz: ubos_core::consts::Mining::DEFAULT_FREQUENCY.mhz(),
    cooling: BoardCooling {
        target_temp_c: 67.0,
        min_fan_pwm: 20,
    },
    model: "miniminer",
};

/// Hashboard descriptor for the 4-chip BM1370 hashnode board. Four BM1370
/// voltage domains stack in series; the femto reference design ran two
/// domains at 2.4 V total (≈1.2 V per domain), so four need ≈4.8 V.
pub const HASHNODE: BoardDescriptor = BoardDescriptor {
    chip: Chip::Bm1370,
    num_voltage_domains: 4,
    num_chips: 4,
    max_psu_voltage: Voltage::from_mv(4800),
    // Full series-stacked rail tuning policy; ceiling matches the PSU limit.
    tuning_voltage_min_mv: 4000,
    tuning_voltage_max_mv: 4800,
    default_voltage_mv: 4550,
    default_frequency_mhz: 600,
    cooling: BoardCooling {
        target_temp_c: 62.0,
        min_fan_pwm: 20,
    },
    model: "Braiins Forge Miner x4",
};
