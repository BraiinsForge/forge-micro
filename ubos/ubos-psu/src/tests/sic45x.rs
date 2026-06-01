#[cfg(test)]
mod math_tests {
    use crate::sic45x::*;

    #[test]
    fn encode_vout_600mv() {
        assert_eq!(encode_vout(600), 0x0133);
    }

    #[test]
    fn encode_vout_1000mv() {
        assert_eq!(encode_vout(1000), 0x0200);
    }

    #[test]
    fn decode_vout_mv_round_trips() {
        assert_eq!(decode_vout_mv(0x0200), 1000);
        assert_eq!(decode_vout_mv(0x0133), 600);
        assert_eq!(decode_vout_mv(0x0300), 1500);
    }

    #[test]
    fn encode_vout_1500mv() {
        assert_eq!(encode_vout(1500), 0x0300);
    }

    #[test]
    fn encode_vout_2650mv() {
        assert_eq!(encode_vout(2650), 0x054D);
    }

    #[test]
    fn encode_vout_350mv() {
        assert_eq!(encode_vout(350), 0x00B3);
    }

    #[test]
    fn encode_vout_540mv() {
        assert_eq!(encode_vout(540), 0x0114);
    }

    #[test]
    fn encode_vout_510mv() {
        assert_eq!(encode_vout(510), 0x0105);
    }

    #[test]
    fn encode_lin11_half_15v() {
        assert_eq!(encode_lin11_half(15_000), 0xF81E);
    }

    #[test]
    fn encode_lin11_half_22v() {
        assert_eq!(encode_lin11_half(22_000), 0xF82C);
    }

    #[test]
    fn encode_lin11_half_18v() {
        assert_eq!(encode_lin11_half(18_000), 0xF824);
    }

    #[test]
    fn encode_lin11_half_9v() {
        assert_eq!(encode_lin11_half(9_000), 0xF812);
    }

    #[test]
    fn encode_lin11_half_25v() {
        assert_eq!(encode_lin11_half(25_000), 0xF832);
    }

    #[test]
    fn encode_lin11_half_20v() {
        assert_eq!(encode_lin11_half(20_000), 0xF828);
    }

    #[test]
    fn encode_lin11_half_5v() {
        assert_eq!(encode_lin11_half(5_000), 0xF80A);
    }

    #[test]
    fn encode_temp_105c() {
        assert_eq!(encode_temp(105), 0x0069);
    }

    #[test]
    fn encode_temp_115c() {
        assert_eq!(encode_temp(115), 0x0073);
    }

    #[test]
    fn encode_temp_95c() {
        assert_eq!(encode_temp(95), 0x005F);
    }

    #[test]
    fn encode_temp_125c() {
        assert_eq!(encode_temp(125), 0x007D);
    }

    #[test]
    fn decode_ulin11_milli_15v() {
        assert_eq!(decode_ulin11_milli(0xF81E), 15_000);
    }

    #[test]
    fn decode_ulin11_milli_100w() {
        assert_eq!(decode_ulin11_milli(0x0832), 100_000);
    }

    #[test]
    fn decode_ulin11_milli_unsigned_mantissa() {
        // 0xD7EC: exp field 0x1A = -6, unsigned mantissa 0x7EC = 2028.
        // 2028 * 2^-6 = 31.6875 W. Signed-mantissa decoding would have
        // wrongly yielded a small negative value (the original bug).
        assert_eq!(decode_ulin11_milli(0xD7EC), 31_687);
    }

    #[test]
    fn decode_ulin11_milli_5v() {
        assert_eq!(decode_ulin11_milli(0xF80A), 5_000);
    }
}

#[cfg(test)]
mod register_tests {
    use crate::sic45x::*;

    #[test]
    fn loop_scale_register_values() {
        assert_eq!(LoopScale::Scale0_125.register(), 0xE801);
        assert_eq!(LoopScale::Scale0_25.register(), 0xE802);
        assert_eq!(LoopScale::Scale0_5.register(), 0xE804);
        assert_eq!(LoopScale::Scale1_0.register(), 0xE808);
    }

    #[test]
    fn loop_scale_default_is_0_5() {
        assert_eq!(LoopScale::default(), LoopScale::Scale0_5);
    }

    #[test]
    fn decode_status_undervoltage_only() {
        // STATUS_BYTE bit3 = VIN_UV → undervoltage.
        let flags = decode_status(0b0000_1000);
        assert!(flags.undervoltage);
        assert!(!flags.overvoltage);
        assert!(!flags.thermal_shutdown);
        assert!(!flags.current_limit);
    }

    #[test]
    fn decode_status_all_set() {
        // bits 5, 4, 3, 2 set.
        let flags = decode_status(0b0011_1100);
        assert!(flags.overvoltage);
        assert!(flags.current_limit);
        assert!(flags.undervoltage);
        assert!(flags.thermal_shutdown);
    }

    #[test]
    fn decode_status_overvoltage_only() {
        let flags = decode_status(1 << 5);
        assert!(flags.overvoltage);
        assert!(!flags.current_limit);
        assert!(!flags.undervoltage);
        assert!(!flags.thermal_shutdown);
    }
}

#[cfg(test)]
mod bus_tests {
    use crate::sic45x::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

    fn run<F: core::future::Future>(future: F) -> F::Output {
        embassy_futures::block_on(future)
    }

    /// Build the full 21-transaction init sequence for FEMTO config.
    fn femto_init_expectations() -> Vec<Transaction> {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;

        let scale = config.loop_scale.register().to_le_bytes();
        let vout_overvoltage = encode_vout(config.vout_ov_fault_millivolts).to_le_bytes();
        let vout_undervoltage = encode_vout(config.vout_uv_fault_millivolts).to_le_bytes();
        let vin_overvoltage = encode_lin11_half(config.vin_ov_fault_millivolts).to_le_bytes();
        let iout_oc = encode_lin11_half(config.iout_oc_fault_milliamps).to_le_bytes();
        let ot_fault = encode_temp(config.ot_fault_celsius).to_le_bytes();
        let vin_undervoltage = encode_lin11_half(config.vin_uv_warn_millivolts).to_le_bytes();
        let iout_warn = encode_lin11_half(config.iout_oc_warn_milliamps).to_le_bytes();
        let ot_warn = encode_temp(config.ot_warn_celsius).to_le_bytes();
        let pg_on = encode_vout(config.power_good_on_millivolts).to_le_bytes();
        let pg_off = encode_vout(config.power_good_off_millivolts).to_le_bytes();
        let vout_cmd = encode_vout(config.initial_millivolts).to_le_bytes();

        vec![
            Transaction::write(address, vec![RESTORE_USER_ALL]),
            Transaction::write(address, vec![CLEAR_FAULTS]),
            Transaction::write(address, vec![VOUT_SCALE_LOOP, scale[0], scale[1]]),
            Transaction::write(
                address,
                vec![
                    VOUT_OV_FAULT_LIMIT,
                    vout_overvoltage[0],
                    vout_overvoltage[1],
                ],
            ),
            Transaction::write(
                address,
                vec![
                    VOUT_UV_FAULT_LIMIT,
                    vout_undervoltage[0],
                    vout_undervoltage[1],
                ],
            ),
            Transaction::write(
                address,
                vec![VIN_OV_FAULT_LIMIT, vin_overvoltage[0], vin_overvoltage[1]],
            ),
            Transaction::write(address, vec![IOUT_OC_FAULT_LIMIT, iout_oc[0], iout_oc[1]]),
            Transaction::write(address, vec![OT_FAULT_LIMIT, ot_fault[0], ot_fault[1]]),
            Transaction::write(
                address,
                vec![VIN_UV_WARN_LIMIT, vin_undervoltage[0], vin_undervoltage[1]],
            ),
            Transaction::write(
                address,
                vec![IOUT_OC_WARN_LIMIT, iout_warn[0], iout_warn[1]],
            ),
            Transaction::write(address, vec![OT_WARN_LIMIT, ot_warn[0], ot_warn[1]]),
            Transaction::write(
                address,
                vec![VOUT_OV_FAULT_RESPONSE, VOUT_OV_FAULT_RESPONSE_DEFAULT],
            ),
            Transaction::write(
                address,
                vec![VOUT_UV_FAULT_RESPONSE, VOUT_UV_FAULT_RESPONSE_DEFAULT],
            ),
            Transaction::write(
                address,
                vec![IOUT_OC_FAULT_RESPONSE, IOUT_OC_FAULT_RESPONSE_DEFAULT],
            ),
            Transaction::write(address, vec![OT_FAULT_RESPONSE, OT_FAULT_RESPONSE_DEFAULT]),
            Transaction::write(
                address,
                vec![VIN_OV_FAULT_RESPONSE, VIN_OV_FAULT_RESPONSE_DEFAULT],
            ),
            Transaction::write(address, vec![POWER_GOOD_ON, pg_on[0], pg_on[1]]),
            Transaction::write(address, vec![POWER_GOOD_OFF, pg_off[0], pg_off[1]]),
            Transaction::write(address, vec![VOUT_COMMAND, vout_cmd[0], vout_cmd[1]]),
            Transaction::write(address, vec![CLEAR_FAULTS]),
        ]
    }

    #[test]
    fn init_writes_full_sequence_for_femto() {
        let expectations = femto_init_expectations();
        let mut bus = Mock::new(&expectations);
        run(init(&mut bus, &Sic45xConfig::FEMTO)).expect("BUG: init failed");
        bus.done();
    }

    #[test]
    fn enable_reads_operation_then_sets_on_bit_then_clears_faults() {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;

        let expectations = [
            Transaction::write_read(address, vec![OPERATION], vec![0x00]),
            Transaction::write(address, vec![OPERATION, OPERATION_ON]),
            Transaction::write(address, vec![CLEAR_FAULTS]),
        ];

        let mut bus = Mock::new(&expectations);
        run(enable(&mut bus, config)).expect("BUG: enable failed");
        bus.done();
    }

    #[test]
    fn disable_reads_operation_then_clears_on_bit() {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;

        let expectations = [
            Transaction::write_read(address, vec![OPERATION], vec![OPERATION_ON]),
            Transaction::write(address, vec![OPERATION, 0x00]),
        ];

        let mut bus = Mock::new(&expectations);
        run(disable(&mut bus, config)).expect("BUG: disable failed");
        bus.done();
    }

    #[test]
    fn set_voltage_out_of_range_does_not_touch_bus() {
        let config = &Sic45xConfig::FEMTO;
        // Below minimum (400 mV).
        let mut bus = Mock::new(&[]);
        let result = run(set_voltage(&mut bus, config, 300));
        bus.done();
        assert!(matches!(result, Err(crate::Error::OutOfRange)));

        // Above maximum (2400 mV).
        let mut bus = Mock::new(&[]);
        let result = run(set_voltage(&mut bus, config, 2_500));
        bus.done();
        assert!(matches!(result, Err(crate::Error::OutOfRange)));
    }

    #[test]
    fn set_voltage_in_range_writes_vout_command() {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;
        let bytes = encode_vout(1000).to_le_bytes();

        let expectations = [Transaction::write(
            address,
            vec![VOUT_COMMAND, bytes[0], bytes[1]],
        )];

        let mut bus = Mock::new(&expectations);
        run(set_voltage(&mut bus, config, 1000)).expect("BUG: set_voltage failed");
        bus.done();
    }

    #[test]
    fn read_status_decodes_status_byte() {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;
        // bit5 = VOUT_OV only.
        let expectations = [Transaction::write_read(
            address,
            vec![STATUS_BYTE],
            vec![0b0010_0000],
        )];

        let mut bus = Mock::new(&expectations);
        let flags = run(read_status(&mut bus, config)).expect("BUG: read_status failed");
        bus.done();
        assert!(flags.overvoltage);
        assert!(!flags.current_limit);
        assert!(!flags.undervoltage);
        assert!(!flags.thermal_shutdown);
    }

    #[test]
    fn read_pout_mw_decodes_known_reading() {
        let config = &Sic45xConfig::FEMTO;
        let address = config.address;
        // 0xF81E encodes 15 W (15000 milli-units); LE wire = [0x1E, 0xF8].
        let expectations = [Transaction::write_read(
            address,
            vec![READ_POUT],
            vec![0x1E, 0xF8],
        )];

        let mut bus = Mock::new(&expectations);
        let mw = run(read_pout_mw(&mut bus, config)).expect("BUG: read_pout_mw failed");
        bus.done();
        assert_eq!(mw, 15_000);
    }
}
