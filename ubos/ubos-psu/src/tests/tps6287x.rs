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

#[cfg(test)]
mod range_tests {
    use crate::tps6287x::*;

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

#[cfg(test)]
mod register_tests {
    use crate::tps6287x::*;

    #[test]
    fn vset_encode_750_mv_vrange2() {
        let dac = VoltageRange::VRange2
            .voltage_to_dac(750)
            .expect("BUG: 750 mv must be in vrange2");
        let bytes = encode_vset(dac);
        assert_eq!(bytes, [REG_VSET, 0x46]);
    }

    #[test]
    fn control1_encode_default_disabled_with_vramp() {
        let bytes = encode_control1(false, VoltageRampSpeed::VRamp1_25);
        // dischen=1 (bit 3), vramp=10 (bits 1:0).
        assert_eq!(bytes, [REG_CONTROL1, 0b0000_1010]);
    }

    #[test]
    fn control1_encode_enabled_with_vramp() {
        let bytes = encode_control1(true, VoltageRampSpeed::VRamp1_25);
        // swen=1 (bit 5), dischen=1 (bit 3), vramp=10 (bits 1:0).
        assert_eq!(bytes, [REG_CONTROL1, 0b0010_1010]);
    }

    #[test]
    fn control1_encode_reset_clears_vramp_field_to_zero() {
        let bytes = encode_control1_reset();
        // reset bit only.
        assert_eq!(bytes, [REG_CONTROL1, 0b1000_0000]);
    }

    #[test]
    fn control2_encode_vrange2_sstime1() {
        let bytes = encode_control2(VoltageRange::VRange2, SoftStartRampTime::SSTime1);
        // vrange=10 (bits 3:2), sstime=01 (bits 1:0).
        assert_eq!(bytes, [REG_CONTROL2, 0b0000_1001]);
    }

    #[test]
    fn status_decode_undervoltage_only() {
        let flags = decode_status(0b0000_0010);
        assert!(flags.undervoltage);
        assert!(!flags.overvoltage);
        assert!(!flags.thermal_shutdown);
        assert!(!flags.current_limit);
    }

    #[test]
    fn status_decode_all_set() {
        let flags = decode_status(0b0001_0111);
        assert!(flags.undervoltage);
        assert!(flags.overvoltage);
        assert!(flags.thermal_shutdown);
        assert!(flags.current_limit);
    }
}

#[cfg(test)]
mod bus_tests {
    use crate::BmmPsuDescriptor;
    use crate::tps6287x::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

    fn run<F: core::future::Future>(future: F) -> F::Output {
        embassy_futures::block_on(future)
    }

    #[test]
    fn init_writes_reset_then_control1_then_control2_then_vset() {
        let config = BmmPsuDescriptor::DEFAULT;
        let address = config.dcdc_address;
        let dac = config
            .dcdc_voltage_range
            .voltage_to_dac(config.dcdc_initial.mv())
            .expect("BUG: initial mv must be in vrange2");

        let expectations = [
            Transaction::write(address, encode_control1_reset().to_vec()),
            Transaction::write(
                address,
                encode_control1(false, config.dcdc_ramp_speed).to_vec(),
            ),
            Transaction::write(
                address,
                encode_control2(config.dcdc_voltage_range, config.dcdc_soft_start).to_vec(),
            ),
            Transaction::write(address, encode_vset(dac).to_vec()),
        ];

        let mut bus = Mock::new(&expectations);
        run(init(&mut bus, &config)).expect("BUG: init failed");
        bus.done();
    }

    #[test]
    fn enable_writes_control1_with_swen_and_polls_status() {
        let config = BmmPsuDescriptor::DEFAULT;
        let address = config.dcdc_address;

        let expectations = [
            Transaction::write(
                address,
                encode_control1(true, config.dcdc_ramp_speed).to_vec(),
            ),
            Transaction::write_read(address, vec![REG_STATUS], vec![0x00]),
        ];

        let mut bus = Mock::new(&expectations);
        run(enable(&mut bus, &config)).expect("BUG: enable failed");
        bus.done();
    }

    #[test]
    fn enable_swallows_status_fault_to_clear_latched_bits() {
        // Soft-start ramp latches undervoltage briefly; reading
        // STATUS clears the bit and enable returns Ok regardless.
        let config = BmmPsuDescriptor::DEFAULT;
        let address = config.dcdc_address;

        let expectations = [
            Transaction::write(
                address,
                encode_control1(true, config.dcdc_ramp_speed).to_vec(),
            ),
            Transaction::write_read(address, vec![REG_STATUS], vec![0b0000_0010]),
        ];

        let mut bus = Mock::new(&expectations);
        run(enable(&mut bus, &config)).expect("BUG: enable must swallow latched status bits");
        bus.done();
    }

    #[test]
    fn set_voltage_out_of_range_does_not_touch_bus() {
        let config = BmmPsuDescriptor::DEFAULT;
        let mut bus = Mock::new(&[]);
        let result = run(set_voltage(&mut bus, &config, 2_000));
        bus.done();
        assert!(matches!(result, Err(crate::Error::OutOfRange)));
    }

    #[test]
    fn set_voltage_writes_vset() {
        let config = BmmPsuDescriptor::DEFAULT;
        let address = config.dcdc_address;
        let dac = config
            .dcdc_voltage_range
            .voltage_to_dac(700)
            .expect("BUG: 700 mv must be in vrange2");

        let expectations = [Transaction::write(address, encode_vset(dac).to_vec())];

        let mut bus = Mock::new(&expectations);
        run(set_voltage(&mut bus, &config, 700)).expect("BUG: set_voltage failed");
        bus.done();
    }

    #[test]
    fn disable_writes_control1_with_swen_cleared() {
        let config = BmmPsuDescriptor::DEFAULT;
        let address = config.dcdc_address;

        let expectations = [Transaction::write(
            address,
            encode_control1(false, config.dcdc_ramp_speed).to_vec(),
        )];

        let mut bus = Mock::new(&expectations);
        run(disable(&mut bus, &config)).expect("BUG: disable failed");
        bus.done();
    }
}
