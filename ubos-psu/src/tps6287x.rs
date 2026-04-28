//! TPS6287x DC/DC converter register layout and bus operations.

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
            VoltageRange::VRange0
            | VoltageRange::VRange1
            | VoltageRange::VRange2 => 400,
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
}

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn vrange2_bounds() {
        assert_eq!(VoltageRange::VRange2.start_mv(), 400);
        assert_eq!(VoltageRange::VRange2.end_mv(), 1675);
        assert_eq!(VoltageRange::VRange2.step_microvolts(), 5_000);
    }

    #[test]
    fn voltage_to_dac_in_range() {
        let r = VoltageRange::VRange2;
        assert_eq!(r.voltage_to_dac(400), Some(0x00));
        assert_eq!(r.voltage_to_dac(750), Some(0x46));
        assert_eq!(r.voltage_to_dac(1675), Some(0xff));
    }

    #[test]
    fn voltage_to_dac_out_of_range() {
        let r = VoltageRange::VRange2;
        assert_eq!(r.voltage_to_dac(375), None);
        assert_eq!(r.voltage_to_dac(1700), None);
    }

    #[test]
    fn voltage_to_dac_rounds_to_nearest_step() {
        let r = VoltageRange::VRange2;
        // 752 mV is between 750 (0x46) and 755 (0x47); rounds to 750.
        assert_eq!(r.voltage_to_dac(752), Some(0x46));
        // 753 mV rounds up to 755 (0x47).
        assert_eq!(r.voltage_to_dac(753), Some(0x47));
    }

    #[test]
    fn dac_to_mv_round_trip_endpoints() {
        let r = VoltageRange::VRange2;
        assert_eq!(r.dac_to_mv(0x00), 400);
        assert_eq!(r.dac_to_mv(0xff), 1675);
    }

    #[test]
    fn vrange0_sub_mv_step_loses_precision_intentionally() {
        // VRange0 step is 1.25 mV; integer mV API rounds to nearest.
        let r = VoltageRange::VRange0;
        assert_eq!(r.start_mv(), 400);
        assert_eq!(r.step_microvolts(), 1_250);
        // 401 mV is closer to DAC=1 (401.25 mV) than DAC=0 (400 mV).
        assert_eq!(r.voltage_to_dac(401), Some(1));
    }
}
