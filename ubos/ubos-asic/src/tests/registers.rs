use super::*;

#[test]
fn control_command_chain_inactive_packs_to_broadcast() {
    // chain_inactive() is the broadcast preamble issued three times at
    // bring-up to drop assigned addresses; the encoder reads code,
    // broadcast, and hw_addr off the value, so accidental field churn
    // surfaces here before it reaches the wire.
    let cmd = ControlCommand::chain_inactive();
    assert_eq!(cmd.code(), ControlCode::ChainInactive);
    assert!(cmd.is_broadcast());
    assert_eq!(cmd.hw_addr(), 0x00);
}

#[test]
fn control_command_set_chip_address_packs_to_unicast() {
    // set_chip_address(addr) is unicast (broadcasting would assign the
    // same address to every chip); the caller walks one per slot.
    let cmd = ControlCommand::set_chip_address(0x02);
    assert_eq!(cmd.code(), ControlCode::SetChipAddress);
    assert!(!cmd.is_broadcast());
    assert_eq!(cmd.hw_addr(), 0x02);
}

#[test]
fn control_code_numeric_values_match_spec() {
    // The cmd byte's low 3 bits hold the control code; the protocol
    // pins SetChipAddress=0 and ChainInactive=3.
    assert_eq!(ControlCode::SetChipAddress.as_u8(), 0);
    assert_eq!(ControlCode::ChainInactive.as_u8(), 3);
}

#[test]
fn register_addresses_match_specification() {
    assert_eq!(ChipIdReg::REG_NUM, 0x00);
    assert_eq!(PllParameterReg::REG_NUM, 0x08);
    assert_eq!(ChipNonceOffsetReg::REG_NUM, 0x0c);
    assert_eq!(HashCountingNumberReg::REG_NUM, 0x10);
    assert_eq!(TicketMaskReg::REG_NUM, 0x14);
    assert_eq!(MiscCtrlReg::REG_NUM, 0x18);
    assert_eq!(BaudRateReg::REG_NUM, 0x28);
    assert_eq!(CoreControlReg::REG_NUM, 0x3c);
    assert_eq!(CoreRegisterValueReg::REG_NUM, 0x40);
    assert_eq!(ExternalTemperatureSensorReg::REG_NUM, 0x44);
    assert_eq!(NonceErrorCounterReg::REG_NUM, 0x4c);
    assert_eq!(AnalogMuxControl::REG_NUM, 0x54);
    assert_eq!(IoDriverStrengthCtrlReg::REG_NUM, 0x58);
    assert_eq!(Pll0DividerReg::REG_NUM, 0x70);
    assert_eq!(Domain0NonceCounterReg::REG_NUM, 0x88);
    assert_eq!(Domain1NonceCounterReg::REG_NUM, 0x89);
    assert_eq!(Domain2NonceCounterReg::REG_NUM, 0x8a);
    assert_eq!(Domain3NonceCounterReg::REG_NUM, 0x8b);
    assert_eq!(TotalNonceCounterReg::REG_NUM, 0x8c);
    assert_eq!(MidstateCalReg::REG_NUM, 0xa4);
    assert_eq!(SoftResetControlReg::REG_NUM, 0xa8);
}

#[test]
fn ticket_mask_from_difficulty_encodes_diff_one_as_zero() {
    // Difficulty 1 accepts every share; mask must be zero so no hash
    // bits are required to match.
    let reg = TicketMaskReg::from_difficulty(1).expect("BUG: diff 1 must encode");
    assert_eq!(reg.ticket_mask(), 0);
}

#[test]
fn ticket_mask_from_difficulty_rejects_invalid() {
    assert!(TicketMaskReg::from_difficulty(0).is_none());
    assert!(TicketMaskReg::from_difficulty(3).is_none());
    assert!(TicketMaskReg::from_difficulty(2047).is_none());
}

#[test]
fn ticket_mask_from_difficulty_encodes_higher_diff() {
    // Difficulty 2 accepts one in two shares: the mask has its top
    // bit set after the reverse_bits/swap_bytes transform.
    let reg = TicketMaskReg::from_difficulty(2).expect("BUG: diff 2 must encode");
    assert_eq!(reg.ticket_mask(), 0x0000_0080);
}

#[test]
fn misc_ctrl_default_bringup_round_trips() {
    // The bring-up baseline must round-trip byte-exact through the
    // typed layout; reserved-bit drift would surface here.
    let bytes = MISC_CTRL_DEFAULT_BRINGUP.to_be_bytes();
    let reg = MiscCtrlReg::from_be_bytes(bytes);
    assert_eq!(reg.to_be_bytes(), bytes);
    let from_helper = MiscCtrlReg::default_bringup_config();
    assert_eq!(from_helper.to_be_bytes(), bytes);
}

#[test]
fn baud_rate_fast_3mbaud_helper_matches_constant() {
    let reg = BaudRateReg::fast_3mbaud();
    let value = u32::from_be_bytes(reg.to_be_bytes());
    assert_eq!(value, BAUDRATE_FAST_3MBAUD);
}

#[test]
fn baud_rate_fast_3mbaud_round_trips() {
    let bytes = BAUDRATE_FAST_3MBAUD.to_be_bytes();
    let reg = BaudRateReg::from_be_bytes(bytes);
    assert_eq!(reg.to_be_bytes(), bytes);
}

#[test]
fn baud_rate_fast_3mbaud_decodes_bt8d_zero() {
    // bt8d = 0 selects the 3 MBaud line rate per the field formula.
    let bytes = BAUDRATE_FAST_3MBAUD.to_be_bytes();
    let reg = BaudRateReg::from_be_bytes(bytes);
    assert_eq!(reg.bt8d(), 0);
}

/// Per-chip nonce offsets exercised during chip-address enumeration:
/// `cnov` always set, the offsets straddle the 16-bit space at chip-
/// quarter boundaries minus a small per-chip skew.
const CHIP_NONCE_OFFSET_CORPUS: &[(u16, u32)] = &[
    (0x0000, 0x8000_0000),
    (0x3fff, 0x8000_3fff),
    (0x7ffe, 0x8000_7ffe),
    (0xbffd, 0x8000_bffd),
];

#[test]
fn chip_nonce_offset_from_offset_round_trips() {
    for &(offset, expected) in CHIP_NONCE_OFFSET_CORPUS {
        let reg = ChipNonceOffsetReg::from_offset(offset);
        let value = u32::from_be_bytes(reg.to_be_bytes());
        assert_eq!(value, expected, "offset 0x{offset:04x} encoded wrong");
        assert!(reg.cnov());
        assert_eq!(reg.offset(), offset);
    }
}

#[test]
fn chip_nonce_offset_decodes_round_trip() {
    for &(expected_offset, raw) in CHIP_NONCE_OFFSET_CORPUS {
        let reg = ChipNonceOffsetReg::from_be_bytes(raw.to_be_bytes());
        assert!(reg.cnov(), "cnov should be set in 0x{raw:08x}");
        assert_eq!(reg.offset(), expected_offset);
    }
}

/// `CoreControl` broadcast writes that bring up the per-core clock /
/// delay / enable settings: `HashClockCtrl=0x40`,
/// `ClockDelayCtrl=0x08`/`0x34`, `CoreReg2=0xaa`. All four
/// use broadcast (`all=1`) with zero core-id / num.
const CORE_CONTROL_CORPUS: &[(CoreRegister, u8, u32)] = &[
    (CoreRegister::HashClockCtrl, 0x40, 0x8000_8540),
    (CoreRegister::ClockDelayCtrl, 0x34, 0x8000_8034),
    (CoreRegister::ClockDelayCtrl, 0x08, 0x8000_8008),
    (CoreRegister::CoreReg2, 0xaa, 0x8000_82aa),
];

#[test]
fn core_control_for_core_write_packs_to_corpus() {
    for &(reg_addr, data, expected) in CORE_CONTROL_CORPUS {
        let reg = CoreControlReg::for_core_write(reg_addr, data);
        let value = u32::from_be_bytes(reg.to_be_bytes());
        assert_eq!(value, expected, "write {reg_addr:?}=0x{data:02x} mispacked");
    }
}

#[test]
fn core_control_default_helpers_match_constants() {
    let sel = CoreControlReg::default_hash_clock_config();
    assert_eq!(
        u32::from_be_bytes(sel.to_be_bytes()),
        CORE_CONTROL_DEFAULT_HASH_CLOCK
    );
    let delay = CoreControlReg::default_clock_delay_config();
    assert_eq!(
        u32::from_be_bytes(delay.to_be_bytes()),
        CORE_CONTROL_DEFAULT_CLOCK_DELAY
    );
    let enable = CoreControlReg::default_core_reg2_config();
    assert_eq!(
        u32::from_be_bytes(enable.to_be_bytes()),
        CORE_CONTROL_DEFAULT_CORE_REG2
    );
}

#[test]
fn core_control_decodes_corpus() {
    for &(expected_addr, expected_data, raw) in CORE_CONTROL_CORPUS {
        let reg = CoreControlReg::from_be_bytes(raw.to_be_bytes());
        assert!(reg.do_cmd(), "corpus writes always set DO_CMD");
        assert!(reg.reg_wr(), "corpus writes are always writes");
        assert_eq!(reg.core_id(), 0);
        assert_eq!(reg.reg_addr(), expected_addr);
        assert_eq!(reg.reg_wdata(), expected_data);
    }
}

#[test]
fn core_register_value_decodes_known_layout() {
    // Construct a synthetic response: core_id=12, found=0xab,
    // value=0x42. The middle byte is FOUND, so keep the whole byte
    // intact rather than splitting it into a register ID.
    let reg = CoreRegisterValueReg::new()
        .with_core_id(12)
        .with_found(0xab)
        .with_core_reg_value(0x42);
    let bytes = reg.to_be_bytes();
    let decoded = CoreRegisterValueReg::from_be_bytes(bytes);
    assert_eq!(decoded.core_id(), 12);
    assert_eq!(decoded.found(), 0xab);
    assert_eq!(decoded.core_reg_value(), 0x42);
}

#[test]
fn opaque_counter_registers_round_trip() {
    // The nonce / temperature / domain-counter registers carry an
    // opaque u32 payload; verify the wrapper preserves all 32 bits.
    let payload: u32 = 0xdead_beef;
    let bytes = payload.to_be_bytes();
    assert_eq!(
        ExternalTemperatureSensorReg::from_be_bytes(bytes).value(),
        payload
    );
    assert_eq!(NonceErrorCounterReg::from_be_bytes(bytes).count(), payload);
    assert_eq!(
        Domain0NonceCounterReg::from_be_bytes(bytes).count(),
        payload
    );
    assert_eq!(
        Domain1NonceCounterReg::from_be_bytes(bytes).count(),
        payload
    );
    assert_eq!(
        Domain2NonceCounterReg::from_be_bytes(bytes).count(),
        payload
    );
    assert_eq!(
        Domain3NonceCounterReg::from_be_bytes(bytes).count(),
        payload
    );
    assert_eq!(TotalNonceCounterReg::from_be_bytes(bytes).count(), payload);
}

#[test]
fn analog_mux_disabled_round_trips() {
    let bytes = ANALOG_MUX_DISABLED.to_be_bytes();
    let reg = AnalogMuxControl::from_be_bytes(bytes);
    assert_eq!(reg.ana_mux_sel(), 0x03);
    assert_eq!(reg.to_be_bytes(), bytes);
    let from_helper = AnalogMuxControl::disabled();
    assert_eq!(from_helper.to_be_bytes(), bytes);
}

#[test]
fn io_driver_strength_default_round_trips() {
    let reg = IoDriverStrengthCtrlReg::default_drive_strengths();
    let value = u32::from_be_bytes(reg.to_be_bytes());
    assert_eq!(value, IO_DRIVER_STRENGTH_DEFAULT);
    assert_eq!(reg.co_ds(), 1);
    assert_eq!(reg.bo_ds(), 1);
    assert_eq!(reg.nrsto_ds(), 1);
    assert_eq!(reg.clko_ds(), 1);
    assert_eq!(reg.ro_ds(), 1);
    assert!(!reg.d0rs_en());
    assert!(!reg.d1rs_en());
    assert!(!reg.d2rs_en());
    assert!(!reg.d3rs_en());
    assert_eq!(reg.rf_ds(), 0);
    assert_eq!(reg.reserved_high(), 0);
}

#[test]
fn pll0_divider_reset_round_trips() {
    let bytes = PLL0_DIVIDER_RESET.to_be_bytes();
    let reg = Pll0DividerReg::from_be_bytes(bytes);
    assert_eq!(reg.wdiv(), 0);
    assert_eq!(reg.unknown_bits_15_8(), 0);
    assert_eq!(reg.unknown_bits_23_16(), 0);
    assert_eq!(reg.unknown_bits_31_24(), 0);
    assert_eq!(reg.to_be_bytes(), bytes);
    let from_helper = Pll0DividerReg::zeroed();
    assert_eq!(from_helper.to_be_bytes(), bytes);
}

#[test]
fn soft_reset_quiescent_and_srst_b1_match_constants() {
    let quiescent = SoftResetControlReg::quiescent();
    assert_eq!(
        u32::from_be_bytes(quiescent.to_be_bytes()),
        SOFT_RESET_QUIESCENT
    );
    let srst_b1 = SoftResetControlReg::new().with_b3_0(0x2);
    assert_eq!(
        u32::from_be_bytes(srst_b1.to_be_bytes()),
        SOFT_RESET_SRST_B1
    );
}

#[test]
fn midstate_cal_factory_default_packs_to_bringup_baseline() {
    // factory_default() encodes to 0x9000_FFFF: bit 31 (en), bit 28
    // (en2), bit 29 clear (unknown_bit_29), bits 15:0 = 0xFFFF (full
    // version-rolled bitmask).
    let reg = MidstateCalReg::factory_default();
    let value = u32::from_be_bytes(reg.to_be_bytes());
    assert_eq!(value, MIDSTATE_CAL_BRINGUP_BASELINE);
    assert!(reg.en());
    assert!(reg.en2());
    assert_eq!(reg.version_rolled_bitmask(), 0xFFFF);
    assert_eq!(reg.reserved_mid(), 0);
}

#[test]
fn soft_reset_quiescent_decodes_as_idle() {
    // 0x0000_0000 leaves every reset bit clear and raw byte 23:16 at 0.
    let bytes = SOFT_RESET_QUIESCENT.to_be_bytes();
    let reg = SoftResetControlReg::from_be_bytes(bytes);
    assert_eq!(reg.b3_0(), 0);
    assert_eq!(reg.b7_4(), 0);
    assert!(!reg.b8());
    assert!(!reg.b9());
    assert_eq!(reg.b23_16(), 0);
    assert_eq!(reg.to_be_bytes(), bytes);
}

#[test]
fn soft_reset_srst_b1_decodes_as_low_nibble_bit1() {
    // 0x0000_0002 sets bit 1 of the low selector nibble; every other
    // field stays zero.
    let bytes = SOFT_RESET_SRST_B1.to_be_bytes();
    let reg = SoftResetControlReg::from_be_bytes(bytes);
    assert_eq!(reg.b3_0(), 0x2);
    assert_eq!(reg.b7_4(), 0);
    assert!(!reg.b8());
    assert!(!reg.b9());
    assert_eq!(reg.b23_16(), 0);
    assert_eq!(reg.to_be_bytes(), bytes);
}

#[test]
fn soft_reset_selector_nibbles_round_trip() {
    // The selector nibbles are raw 4-bit fields; every value 0x0..=0xf
    // must survive a round trip through both nibbles independently.
    for mask in 0x0u8..=0xf {
        let reg = SoftResetControlReg::new().with_b3_0(mask).with_b7_4(mask);
        let decoded = SoftResetControlReg::from_be_bytes(reg.to_be_bytes());
        assert_eq!(decoded.b3_0(), mask);
        assert_eq!(decoded.b7_4(), mask);
    }
}

#[test]
fn core_register_unmapped_id_preserves_bits() {
    // Read responses from chip-to-host frames can carry register IDs
    // outside the four named variants; from_bits wraps them in
    // Unknown(value) so the 5-bit payload survives the round trip.
    for unmapped in [0x01, 0x03, 0x04, 0x0c, 0x0f, 0x10, 0x1f] {
        let decoded = CoreRegister::from_bits(unmapped);
        assert_eq!(decoded, CoreRegister::Unknown(unmapped));
        assert_eq!(decoded.into_bits(), unmapped);
    }
}
