use super::*;

use ubos_core::units::Voltage;

#[test]
fn bmm100_max_rail_voltage_clamps_to_psu_ceiling() {
    // Chip stack 4 * 422 mV = 1688 mV exceeds the BMM100 PSU's
    // configured upper bound 1675 mV; max_rail_voltage clamps to the
    // PSU ceiling so Psu::set_voltage never returns Error::OutOfRange
    // when the tuner anchors the open-core rail at this value.
    assert_eq!(BMM100.max_rail_voltage(), Voltage::from_mv(1675));
}
