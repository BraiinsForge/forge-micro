use super::*;

#[test]
fn frequency_from_mhz_saturates_above_u32_max_khz() {
    // Inputs above u32::MAX / 1000 would overflow the kHz scalar.
    // saturating_mul caps at u32::MAX rather than wrapping, so a
    // corrupted input cannot present as a plausibly low clock.
    let max_mhz = u32::MAX / 1000;
    assert_eq!(Frequency::from_mhz(max_mhz).khz(), max_mhz * 1000);
    assert_eq!(Frequency::from_mhz(max_mhz + 1).khz(), u32::MAX);
    assert_eq!(Frequency::from_mhz(u32::MAX).khz(), u32::MAX);
}

#[test]
fn frequency_range_reports_its_inclusive_bounds() {
    // The descriptors type their operating envelope with this range and
    // the register calculators read both bounds back to reject targets
    // outside it, so min/max must return the values new() was handed.
    let range = FrequencyRange::new(Frequency::from_mhz(50), Frequency::from_mhz(1000));
    assert_eq!(range.min(), Frequency::from_mhz(50));
    assert_eq!(range.max(), Frequency::from_mhz(1000));
}
