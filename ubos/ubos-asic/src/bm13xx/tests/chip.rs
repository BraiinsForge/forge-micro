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

use super::*;

use crate::bm13xx::framing::{ChipAddress, ControlCommand};
use crate::bm13xx::registers::{
    BaudRateReg, ChipIdReg, ChipNonceOffsetReg, CoreControlReg, CoreRegister,
    IO_DRIVER_STRENGTH_DOMAIN_BOUNDARY, IoDriverStrengthCtrlReg, MISC_SETTINGS_BM1370_BRINGUP,
    MidstateCalReg, MiscCtrlReg, MiscSettingsReg, PllParameterReg, Register, SoftResetControlReg,
    UartRelayReg,
};
use crate::test_support::block_on;
use crate::{BMM101_HASHBOARD, HashboardDescriptor};

/// One recorded bus operation. Addresses are encoded as a `u16` so the
/// broadcast address (`0x100`) never collides with a chip address.
#[derive(Debug, PartialEq, Eq)]
enum Op {
    Write {
        addr: u16,
        reg: u8,
        value: u32,
    },
    Read {
        addr: u16,
        reg: u8,
    },
    Command {
        code: u8,
        broadcast: bool,
        hw_addr: u8,
    },
    Delay(u32),
}

fn encode_addr(addr: ChipAddress) -> u16 {
    match addr {
        ChipAddress::All => 0x100,
        ChipAddress::Chip(address) => u16::from(address),
    }
}

/// Builds the chip-id register payload a responding chip reports for
/// the given `chip_id` value.
fn make_chip_id(chip_id: u32) -> u32 {
    // The chip-id field occupies the top 16 bits of ChipIdReg; build
    // the payload via the typed constructor so field layout changes
    // surface here automatically.
    #[allow(clippy::cast_possible_truncation)]
    u32::from_be_bytes(ChipIdReg::new().with_chip_id(chip_id as u16).to_be_bytes())
}

/// Records every bus call so a test can assert the bring-up sequence.
/// `chip_id` controls what `read_register` returns for `ChipIdReg`
/// reads, allowing tests to simulate different chip models.
struct MockBus {
    ops: heapless::Vec<Op, 512>,
    responders: u8,
    chip_id: u32,
}

impl MockBus {
    /// Creates a bus that returns `responders` chip-id replies with
    /// `chip_id` as the model identifier on every read.
    fn new(responders: u8, chip_id: u32) -> Self {
        Self {
            ops: heapless::Vec::new(),
            responders,
            chip_id,
        }
    }

    fn push(&mut self, op: Op) {
        // Generous capacity; a drop here would fail the assertions anyway.
        let _ = self.ops.push(op);
    }
}

impl ChipBus for MockBus {
    async fn write_register_raw(&mut self, addr: ChipAddress, reg: u8, value: u32) {
        self.push(Op::Write {
            addr: encode_addr(addr),
            reg,
            value,
        });
    }

    async fn read_register<R: Register>(
        &mut self,
        chip_address: ChipAddress,
    ) -> heapless::Vec<(u8, R), MAX_CHAIN_CHIPS> {
        self.push(Op::Read {
            addr: encode_addr(chip_address),
            reg: R::REG_NUM,
        });
        // Only ChipIdReg reads carry a meaningful payload; any other
        // register reads back zeroed so a misdirected read surfaces as an
        // obviously-empty value rather than chip-id-shaped garbage.
        let bits = if R::REG_NUM == ChipIdReg::REG_NUM {
            make_chip_id(self.chip_id)
        } else {
            0
        };
        let mut responders = heapless::Vec::new();
        for address in 0..self.responders {
            let _ = responders.push((address, R::from_be_bytes(bits.to_be_bytes())));
        }
        responders
    }

    async fn send_command(&mut self, command: ControlCommand) {
        self.push(Op::Command {
            code: command.code().as_u8(),
            broadcast: command.is_broadcast(),
            hw_addr: command.hw_addr(),
        });
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.push(Op::Delay(ms));
    }
}

/// Runs the full bring-up for any chip model against a mock bus seeded
/// with `responders` chips, and returns the recorded operations plus the
/// discovered chip count. The responders report the chip's own
/// descriptor id, so enumeration matches without a hardcoded literal.
fn run_bringup_for<C: ChipProtocol>(
    chip: &C,
    board: HashboardDescriptor,
    responders: u8,
) -> (heapless::Vec<Op, 512>, u8) {
    let mut bus = MockBus::new(responders, u32::from(chip.descriptor().chip_id));
    let count = block_on(async {
        chip.reset_chain(&mut bus).await;
        let count = chip.enumerate_chips(&mut bus).await;
        chip.initialize_domains(&mut bus, &board).await;
        chip.initialize_cores(&mut bus, &board).await;
        chip.split_nonce_space(&mut bus, &board).await;
        chip.enable_fast_uart(&mut bus).await;
        count
    });
    (bus.ops, count)
}

/// Runs the full BM1362 bring-up against the `BMM101_HASHBOARD` board.
fn run_bringup(responders: u8) -> (heapless::Vec<Op, 512>, u8) {
    run_bringup_for(&Bm1362, BMM101_HASHBOARD, responders)
}

/// Synthetic eight-chip, two-domain hashboard for BM1366 multi-domain
/// tests. Four chips per domain; addressing stride is 1.
const BMM_TEST_MULTI_DOMAIN: HashboardDescriptor = HashboardDescriptor {
    chip: &BM1366.chip,
    num_voltage_domains: 2,
    num_chips: 8,
    max_psu_voltage: ubos_core::units::Voltage::from_mv(5000),
    default_voltage: ubos_core::units::Voltage::from_mv(0),
    default_frequency: ubos_core::units::Frequency::from_mhz(0),
    default_efficiency: ubos_core::units::Efficiency::from_mj_per_th(core::num::NonZeroU32::MIN),
    swarm_color: "blue",
    cooling: crate::board::BoardCooling {
        target_chip_temp: None,
        target_hashboard_temp: Some(ubos_core::units::Temperature::from_celsius(0)),
        min_fan_pwm: 0,
        input_smoother_half_decay_secs: 0,
    },
    dangerous_chip_temp: Some(ubos_core::units::Temperature::from_celsius(110)),
    dangerous_hashboard_temp: Some(ubos_core::units::Temperature::from_celsius(90)),
    model: "test",
    eeprom_i2c_address: None,
};

/// Single-chip BM1370 hashboard used in the `MiscSettings` bring-up
/// test. Topology is intentionally minimal; the 0xb9 write does not
/// depend on chip count or domain count.
const BM1370_TEST_BOARD: HashboardDescriptor = HashboardDescriptor {
    chip: &BM1370.chip,
    num_voltage_domains: 1,
    num_chips: 1,
    max_psu_voltage: ubos_core::units::Voltage::from_mv(5000),
    default_voltage: ubos_core::units::Voltage::from_mv(0),
    default_frequency: ubos_core::units::Frequency::from_mhz(0),
    default_efficiency: ubos_core::units::Efficiency::from_mj_per_th(core::num::NonZeroU32::MIN),
    swarm_color: "blue",
    cooling: crate::board::BoardCooling {
        target_chip_temp: None,
        target_hashboard_temp: Some(ubos_core::units::Temperature::from_celsius(0)),
        min_fan_pwm: 0,
        input_smoother_half_decay_secs: 0,
    },
    dangerous_chip_temp: Some(ubos_core::units::Temperature::from_celsius(110)),
    dangerous_hashboard_temp: Some(ubos_core::units::Temperature::from_celsius(90)),
    model: "test",
    eeprom_i2c_address: None,
};

/// Runs the full BM1366 bring-up against the multi-domain test board.
fn run_bringup_bm1366(responders: u8) -> (heapless::Vec<Op, 512>, u8) {
    run_bringup_for(&Bm1366, BMM_TEST_MULTI_DOMAIN, responders)
}

/// Runs the full BM1370 bring-up against the single-chip test board.
fn run_bringup_bm1370(responders: u8) -> (heapless::Vec<Op, 512>, u8) {
    run_bringup_for(&Bm1370, BM1370_TEST_BOARD, responders)
}

// ---------------------------------------------------------------------------
// BM1362 / BMM101_HASHBOARD tests (unchanged behavior)
// ---------------------------------------------------------------------------

#[test]
fn enumerate_counts_responders_with_matching_chip_id() {
    let (_, count) = run_bringup(4);
    assert_eq!(count, 4);
}

#[test]
fn version_rolling_precedes_the_first_read() {
    // Enabling version rolling switches responses to the 11-byte form
    // the parser and reads depend on, so it must come before any read.
    let (ops, _) = run_bringup(4);
    let midstate = ops
        .iter()
        .position(|op| matches!(op, Op::Write { reg, .. } if *reg == MidstateCalReg::REG_NUM))
        .expect("BUG: version-rolling write missing");
    let first_read = ops
        .iter()
        .position(|op| matches!(op, Op::Read { .. }))
        .expect("BUG: enumeration read missing");
    assert!(
        midstate < first_read,
        "version rolling must precede the first read"
    );
}

#[test]
fn baudrate_is_the_last_register_write() {
    let (ops, _) = run_bringup(4);
    let last_write = ops
        .iter()
        .rposition(|op| matches!(op, Op::Write { .. }))
        .expect("BUG: no register writes recorded");
    assert!(
        matches!(&ops[last_write], Op::Write { reg, .. } if *reg == BaudRateReg::REG_NUM),
        "the baudrate switch must be the last register write"
    );
}

#[test]
fn no_pll_write_during_bringup() {
    // Frequency is set by the ramp, not bring-up; the fast UART is
    // crystal-clocked, so no PLL is configured here.
    let (ops, _) = run_bringup(4);
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::Write { reg, .. } if *reg == PllParameterReg::REG_NUM)),
        "bring-up must not program the PLL"
    );
}

#[test]
fn nonce_offsets_are_computed_per_chip() {
    // Four chips split the 16-bit offset space into quarters minus the
    // per-chip skew: 0x0000, 0x3fff, 0x7ffe, 0xbffd. Each register sets
    // the cnov bit (0x8000_0000) on top of the offset.
    let (ops, _) = run_bringup(4);
    let offsets: heapless::Vec<u32, MAX_CHAIN_CHIPS> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Write { reg, value, .. } if *reg == ChipNonceOffsetReg::REG_NUM => Some(*value),
            _ => None,
        })
        .collect();
    let expected: [u32; 4] = [0x8000_0000, 0x8000_3fff, 0x8000_7ffe, 0x8000_bffd];
    assert_eq!(offsets.as_slice(), &expected);

    // Properties independent of the formula above, so a sign or
    // ordering regression in split_nonce_space fails even if the
    // hardcoded expectations were edited to match it: every chip sets
    // the cnov bit and the offsets strictly increase down the chain.
    assert!(offsets.iter().all(|value| value & 0x8000_0000 != 0));
    assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
}

/// Regression: a zero-chip board must return from the nonce split
/// without dividing the offset space by the chip count, and without
/// writing any nonce-offset register. Pins the empty-chain guard so a
/// refactor cannot reintroduce the division by zero.
#[test]
fn nonce_split_on_an_empty_chain_writes_nothing() {
    let mut board = BMM101_HASHBOARD;
    board.num_chips = 0;
    let mut bus = MockBus::new(0, u32::from(BM1362.chip_id));
    block_on(Bm1362.split_nonce_space(&mut bus, &board));
    assert!(
        bus.ops.is_empty(),
        "empty chain must not program nonce offsets"
    );
}

/// Regression: the `BMM101_HASHBOARD` (four chips, four domains, chips-per-domain=1)
/// must not emit any `UartRelayReg` (0x2c) writes; single-chip-per-domain
/// chains need no relay programming.
#[test]
fn bm1362_single_chip_per_domain_emits_no_uart_relay_writes() {
    let (ops, _) = run_bringup(4);
    let relay_writes: usize = ops
        .iter()
        .filter(|op| matches!(op, Op::Write { reg, .. } if *reg == UartRelayReg::REG_NUM))
        .count();
    assert_eq!(
        relay_writes, 0,
        "single-chip-per-domain board must not program the UART relay"
    );
}

// ---------------------------------------------------------------------------
// BM1366 multi-domain tests
// ---------------------------------------------------------------------------

#[test]
fn bm1366_enumerate_counts_responders_with_matching_chip_id() {
    // Eight responders all report chip_id 0x1366; enumeration must count
    // all eight after filtering by the BM1366 descriptor id.
    let (_, count) = run_bringup_bm1366(8);
    assert_eq!(count, 8);
}

#[test]
fn bm1366_domain_init_writes_uart_relay_at_expected_addresses() {
    // Two domains, four chips each.
    // Domain 0: first_chip=0, last_chip=3.
    // Domain 1: first_chip=4, last_chip=7.
    // Each domain writes relay to first and last chip addresses.
    let (ops, _) = run_bringup_bm1366(8);
    let relay_writes: heapless::Vec<(u16, u32), 16> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Write { addr, reg, value } if *reg == UartRelayReg::REG_NUM => {
                Some((*addr, *value))
            }
            _ => None,
        })
        .collect();

    // Domain 0: first=0, last=3 — two relay writes.
    // Domain 1: first=4, last=7 — two relay writes.
    // Total: four relay writes.
    assert_eq!(
        relay_writes.len(),
        4,
        "expected four relay writes for two domains"
    );

    let relay_addrs: heapless::Vec<u16, 16> = relay_writes.iter().map(|(addr, _)| *addr).collect();

    // Domain 0 addresses.
    assert!(
        relay_addrs.contains(&0),
        "domain 0 first chip (addr 0) must receive a relay write"
    );
    assert!(
        relay_addrs.contains(&3),
        "domain 0 last chip (addr 3) must receive a relay write"
    );

    // Domain 1 addresses.
    assert!(
        relay_addrs.contains(&4),
        "domain 1 first chip (addr 4) must receive a relay write"
    );
    assert!(
        relay_addrs.contains(&7),
        "domain 1 last chip (addr 7) must receive a relay write"
    );
}

#[test]
fn bm1366_domain_init_gap_cnt_matches_formula() {
    // chips_per_domain = 4, num_domains = 2.
    // Domain 0: gap_cnt = 4 * (2 - 0) + 14 = 22.
    // Domain 1: gap_cnt = 4 * (2 - 1) + 14 = 18.
    let (ops, _) = run_bringup_bm1366(8);
    let relay_values: heapless::Vec<(u16, u16), 16> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Write { addr, reg, value } if *reg == UartRelayReg::REG_NUM => {
                let decoded = UartRelayReg::from_be_bytes(value.to_be_bytes());
                Some((*addr, decoded.gap_cnt()))
            }
            _ => None,
        })
        .collect();

    // All domain-0 relay writes (addrs 0 and 3) carry gap_cnt = 22.
    for (addr, gap_cnt) in relay_values.iter().copied() {
        if addr == 0 || addr == 3 {
            assert_eq!(
                gap_cnt, 22,
                "domain 0 chip addr {addr}: expected gap_cnt 22, got {gap_cnt}"
            );
        }
    }

    // All domain-1 relay writes (addrs 4 and 7) carry gap_cnt = 18.
    for (addr, gap_cnt) in relay_values.iter().copied() {
        if addr == 4 || addr == 7 {
            assert_eq!(
                gap_cnt, 18,
                "domain 1 chip addr {addr}: expected gap_cnt 18, got {gap_cnt}"
            );
        }
    }
}

#[test]
fn bm1366_domain_init_writes_domain_boundary_drive_strength_to_last_chip() {
    // The last chip of each domain must receive IoDriverStrengthCtrlReg
    // encoding IO_DRIVER_STRENGTH_DOMAIN_BOUNDARY (0x0001_F111).
    // Domain 0 last chip = addr 3; domain 1 last chip = addr 7.
    let (ops, _) = run_bringup_bm1366(8);
    let boundary_writes: heapless::Vec<u16, 16> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Write { addr, reg, value }
                if *reg == IoDriverStrengthCtrlReg::REG_NUM
                    && *value == IO_DRIVER_STRENGTH_DOMAIN_BOUNDARY =>
            {
                Some(*addr)
            }
            _ => None,
        })
        .collect();

    assert!(
        boundary_writes.contains(&3),
        "domain 0 last chip (addr 3) must receive domain-boundary drive strength"
    );
    assert!(
        boundary_writes.contains(&7),
        "domain 1 last chip (addr 7) must receive domain-boundary drive strength"
    );
}

// ---------------------------------------------------------------------------
// BM1370 tests
// ---------------------------------------------------------------------------

#[test]
fn bm1370_writes_misc_settings_after_core_init() {
    // The BM1370 bring-up must write MiscSettingsReg (0xb9) exactly
    // MISC_SETTINGS_REPEATS (2) times, each to the broadcast address
    // with value MISC_SETTINGS_BM1370_BRINGUP (0x0000_4480), and all
    // such writes must appear after the last CoreControlReg (0x3c) write.
    let (ops, _) = run_bringup_bm1370(1);

    let misc_indices: heapless::Vec<usize, 8> = ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| match op {
            Op::Write { reg, .. } if *reg == MiscSettingsReg::REG_NUM => Some(index),
            _ => None,
        })
        .collect();

    assert_eq!(
        misc_indices.len(),
        MISC_SETTINGS_REPEATS,
        "expected exactly {MISC_SETTINGS_REPEATS} MiscSettings writes"
    );

    for index in misc_indices.iter().copied() {
        match &ops[index] {
            Op::Write { addr, value, .. } => {
                assert_eq!(
                    *addr, 0x100,
                    "MiscSettings write must target the broadcast address"
                );
                assert_eq!(
                    *value, MISC_SETTINGS_BM1370_BRINGUP,
                    "MiscSettings write must carry value 0x{MISC_SETTINGS_BM1370_BRINGUP:08x}"
                );
            }
            _ => panic!("BUG: index does not point to a Write op"),
        }
    }

    let last_core_control_index = ops
        .iter()
        .rposition(|op| matches!(op, Op::Write { reg, .. } if *reg == CoreControlReg::REG_NUM))
        .expect("BUG: no CoreControlReg writes recorded");

    let first_misc_index = misc_indices[0];
    assert!(
        first_misc_index > last_core_control_index,
        "MiscSettings writes must follow the last CoreControlReg write"
    );
}

#[test]
fn bm1362_writes_no_misc_settings() {
    // Regression: BM1362 bring-up must not write MiscSettingsReg (0xb9).
    let (ops, _) = run_bringup(4);
    let misc_writes = ops
        .iter()
        .filter(|op| matches!(op, Op::Write { reg, .. } if *reg == MiscSettingsReg::REG_NUM))
        .count();
    assert_eq!(misc_writes, 0, "BM1362 bring-up must not write reg 0xb9");
}

#[test]
fn bm1366_writes_no_misc_settings() {
    // Regression: BM1366 bring-up must not write MiscSettingsReg (0xb9).
    let (ops, _) = run_bringup_bm1366(8);
    let misc_writes = ops
        .iter()
        .filter(|op| matches!(op, Op::Write { reg, .. } if *reg == MiscSettingsReg::REG_NUM))
        .count();
    assert_eq!(misc_writes, 0, "BM1366 bring-up must not write reg 0xb9");
}

// ---------------------------------------------------------------------------
// Chip enum dispatch tests
// ---------------------------------------------------------------------------

#[test]
fn bmm101_selects_bm1362() {
    // The BMM101_HASHBOARD board descriptor must declare the BM1362 chip model and
    // the vendor spec behind it must carry the expected model identifier.
    assert_eq!(BMM101_HASHBOARD.chip.asic_model, Chip::Bm1362.asic_model());
    assert_eq!(Chip::Bm1362.descriptor().chip_id, 0x1362);
}

#[test]
fn chip_enum_dispatches_post_core_config() {
    // Running initialize_cores through Chip::Bm1370 must emit the
    // MiscSettingsReg (0xb9) write because Bm1370 overrides
    // write_misc_settings. Running the same sequence through
    // Chip::Bm1362 must not emit any 0xb9 write.

    let mut bm1370_bus = MockBus::new(1, 0x1370);
    block_on(async {
        let chip = Chip::Bm1370;
        chip.reset_chain(&mut bm1370_bus).await;
        let _ = chip.enumerate_chips(&mut bm1370_bus).await;
        chip.initialize_domains(&mut bm1370_bus, &BM1370_TEST_BOARD)
            .await;
        chip.initialize_cores(&mut bm1370_bus, &BM1370_TEST_BOARD)
            .await;
    });

    let bm1370_misc_count = bm1370_bus
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Write { reg, .. } if *reg == MiscSettingsReg::REG_NUM))
        .count();
    assert_eq!(
        bm1370_misc_count, MISC_SETTINGS_REPEATS,
        "Chip::Bm1370 must forward write_misc_settings to Bm1370"
    );

    let mut bm1362_bus = MockBus::new(4, 0x1362);
    block_on(async {
        let chip = Chip::Bm1362;
        chip.reset_chain(&mut bm1362_bus).await;
        let _ = chip.enumerate_chips(&mut bm1362_bus).await;
        chip.initialize_domains(&mut bm1362_bus, &BMM101_HASHBOARD)
            .await;
        chip.initialize_cores(&mut bm1362_bus, &BMM101_HASHBOARD)
            .await;
    });

    let bm1362_misc_count = bm1362_bus
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Write { reg, .. } if *reg == MiscSettingsReg::REG_NUM))
        .count();
    assert_eq!(
        bm1362_misc_count, 0,
        "Chip::Bm1362 must not emit any MiscSettings writes"
    );
}

#[test]
fn chip_descriptor_midstate_counts_match_silicon() {
    // BM1362 and BM1366 tag returned work-ids with a 3-bit variant index
    // (8 values); BM1370 tags with a 4-bit index (16 values). The host
    // strides work-ids by this count and divides to recover the originating
    // job, so the values are correctness-critical.
    assert_eq!(BM1362.midstate_count, 8);
    assert_eq!(BM1366.midstate_count, 8);
    assert_eq!(BM1370.midstate_count, 16);
}

// ---------------------------------------------------------------------------
// post_pll_rearm typed-constructor bit-equivalence proofs
//
// Each assertion confirms that the typed builder in post_pll_rearm produces
// the same wire bits as the original capture literal. The original literal
// is the source of truth; if any assertion fails the typed form is wrong,
// not the literal.
// ---------------------------------------------------------------------------

const _: () = assert!(
    SoftResetControlReg::new()
        .with_b7_4(0xf)
        .with_b8(true)
        .with_b23_16(0x07)
        .into_bits()
        == 0x0007_01f0
);

const _: () = assert!(
    MiscCtrlReg::default_bringup_config()
        .with_unknown_bits_31_28(0xf)
        .into_bits()
        == 0xf000_c100
);

const _: () = assert!(
    CoreControlReg::for_core_write(CoreRegister::CoreRegB, 0x00).into_bits() == 0x8000_8b00
);

const _: () = assert!(
    CoreControlReg::for_core_write(CoreRegister::ClockDelayCtrl, 0x0c).into_bits() == 0x8000_800c
);

const _: () = assert!(
    CoreControlReg::for_core_write(CoreRegister::Unknown(0x0d), 0xee).into_bits() == 0x8000_8dee
);
