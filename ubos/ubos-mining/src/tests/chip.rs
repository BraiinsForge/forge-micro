//! Equivalence tests gating `MISC_CTRL_DEFAULT_BRINGUP` against the
//! recorded chip-init step sequence. Fails at `cargo test` time if
//! either the typed `MiscCtrlReg` layout or the constant drifts away
//! from the value the chip is programmed with in the captured trace.

use ubos_asic::{
    HashCountingNumberReg, MISC_CTRL_DEFAULT_BRINGUP, MiscCtrlReg, PllParameterReg, Register,
};

use crate::bmm100_trace::{INIT, Step};

#[test]
fn misc_ctrl_baseline_constant_matches_wire_capture() {
    assert_eq!(MiscCtrlReg::REG_NUM, 0x18);
    assert_eq!(MISC_CTRL_DEFAULT_BRINGUP, 0xb000_c100);
}

#[test]
fn misc_ctrl_baseline_write_present_in_init_trace() {
    let found = INIT.iter().any(|step| {
        matches!(
            step,
            Step::WriteRegister { reg, value, .. }
                if *reg == MiscCtrlReg::REG_NUM && *value == MISC_CTRL_DEFAULT_BRINGUP
        )
    });
    assert!(
        found,
        "MISC_CTRL_DEFAULT_BRINGUP write not found in INIT trace",
    );
}

#[test]
fn baseline_write_precedes_first_pll_write() {
    let baseline_index = INIT
        .iter()
        .position(|step| {
            matches!(
                step,
                Step::WriteRegister { reg, value, .. }
                    if *reg == MiscCtrlReg::REG_NUM && *value == MISC_CTRL_DEFAULT_BRINGUP
            )
        })
        .expect(
            "BUG: baseline write must exist per misc_ctrl_baseline_write_present_in_init_trace",
        );
    let first_pll = INIT.iter().position(|step| {
        matches!(
            step,
            Step::WriteRegister { reg, .. }
                if *reg == PllParameterReg::REG_NUM
                    || *reg == HashCountingNumberReg::REG_NUM
        )
    });
    if let Some(pll_index) = first_pll {
        assert!(
            baseline_index < pll_index,
            "MiscCtrl baseline at INIT[{baseline_index}] must precede the first PLL write at INIT[{pll_index}]",
        );
    }
}
