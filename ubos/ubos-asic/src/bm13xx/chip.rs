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

//! Per-chip bring-up over an abstract bus (sans-IO).
//!
//! [`ChipProtocol`] is the per-chip bring-up recipe: one implementation
//! per chip model, expressed as plain imperative async steps that write
//! registers, send control commands, and wait settle delays. It owns no
//! UART and performs no direct I/O; it drives [`ChipBus`], which the
//! driver's `TransportBus` implements over the consumer's byte-level
//! `Transport`. This keeps the bring-up logic in this pure crate and
//! unit-testable against a mock bus.

use heapless::Vec;
use ubos_core::units::{Frequency, FrequencyRange, Voltage};

use super::framing::{ChipAddress, ControlCommand};
use super::registers::{
    AnalogMuxControl, BaudRateReg, ChipIdReg, ChipNonceOffsetReg, CoreControlReg, CoreRegister,
    DomainSelect, HashCountingNumberReg, IoDriverStrengthCtrlReg, MidstateCalReg, MiscCtrlReg,
    MiscSettingsReg, Pll0DividerReg, PllParameterReg, Register, SoftResetControlReg, UartRelayReg,
};
use crate::HashboardDescriptor;
use crate::chain::{ChipDescriptor, ChipError};

#[cfg(test)]
#[path = "tests/chip.rs"]
mod tests;

/// Maximum chips a chain may hold. Bounds the address assignment during
/// enumeration and the responder buffer. Stays well below `0xFF`, which
/// is the broadcast address, not a valid chip address — so every
/// assigned address (`0x00..MAX_CHAIN_CHIPS`) is a real chip address.
pub const MAX_CHAIN_CHIPS: usize = 32;

/// Size of the 16-bit nonce-offset space the chain divides among chips.
const NONCE_OFFSET_SPACE: u32 = 0x1_0000;

/// `ChainInactive` broadcasts issued at reset to tolerate a single lost
/// frame before address assignment.
const CHAIN_INACTIVE_REPEATS: usize = 3;

/// Settle delay after each `ChainInactive`.
const CHAIN_INACTIVE_GAP_MS: u32 = 300;

/// Settle delay between the broadcast reset-config writes.
const RESET_GAP_MS: u32 = 100;

/// Settle delay between core-config writes, and after address
/// assignment before the discovery read.
const SETTLE_GAP_MS: u32 = 50;

/// Base addend in the relay gap-count formula. Added to the
/// topology-derived term so the relay always inserts a minimum spacing
/// even when only one domain sits ahead of the chain head.
const RELAY_BASE_GAP: u32 = 14;

/// Times the BM1370 `MiscSettings` write is repeated during bring-up so a
/// single lost frame still leaves the register programmed.
const MISC_SETTINGS_REPEATS: usize = 2;

/// The chip-side I/O a [`ChipProtocol`] drives. Implemented by the
/// driver's `TransportBus`, which frames each call onto the consumer's
/// byte-level `Transport`, and by the mock bus in tests. Register writes
/// and control commands are fire-and-forget; only reads can fail.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and `ubos_mining`'s `MinerControl`.
// Send bounds are not required on this single-task embedded target.
#[allow(async_fn_in_trait)]
pub trait ChipBus {
    /// Write a raw `value` to register `reg` on `addr` (a single chip or
    /// the broadcast address). The primitive the bus implementation
    /// provides; recipes use the typed [`Self::write_register`] instead.
    async fn write_register_raw(&mut self, addr: ChipAddress, reg: u8, value: u32);

    /// Read register `R` from `chip_address` (a single chip or the
    /// broadcast address) and return one decoded `(chip_address, R)` per
    /// responding chip — empty if none answer. The implementation drops
    /// replies whose register address does not match `R`.
    async fn read_register<R: Register>(
        &mut self,
        chip_address: ChipAddress,
    ) -> Vec<(u8, R), MAX_CHAIN_CHIPS>;

    /// Send a register-less control command (chain-inactive,
    /// set-chip-address).
    async fn send_command(&mut self, command: ControlCommand);

    /// Wait `ms` milliseconds, for the chip-settle delays the bring-up
    /// recipe inserts between writes.
    async fn delay_ms(&mut self, ms: u32);

    /// Write a typed register: pack it to its big-endian payload and
    /// dispatch through [`Self::write_register_raw`]. Provided so
    /// recipes write `bus.write_register(addr, reg)` directly.
    async fn write_register<R: Register>(&mut self, addr: ChipAddress, reg: R) {
        self.write_register_raw(addr, R::REG_NUM, u32::from_be_bytes(reg.to_be_bytes()))
            .await;
    }
}

/// Bitmain-specific capability constants layered over the vendor-neutral
/// [`ChipDescriptor`]. The neutral half is what the `ChainDriver` seam
/// serves to the tuner; the fields here are register-protocol facts only
/// the `bm13xx` recipes and driver consume.
pub struct BmDescriptor {
    /// Vendor-neutral capability bounds served through the chain seam.
    pub chain: ChipDescriptor,
    /// Model identifier the chip reports in `ChipIdReg.chip_id`
    /// (`0x1362`, `0x1366`, `0x1370`). Enumeration matches the value
    /// read off the chain against this to select the chip.
    pub chip_id: u16,
    /// Broadcast `ClockDelayCtrl` value loaded during core bring-up,
    /// before the per-chip pass writes the final clock delay.
    pub broadcast_clock_delay: u8,
    /// Number of distinct work-id values the chip emits per dispatched
    /// work item. The chip tags each returned solution with a variant
    /// index in the low bits of the echoed work-id; the host strides
    /// work-ids by this count and recovers the originating job by
    /// dividing a returned work-id by it. Eight for the BM1362 and
    /// BM1366, sixteen for the BM1370.
    pub midstate_count: u8,
}

/// Per-chip bring-up recipe. One implementation per chip model
/// (`Bm1362` / `Bm1366` / `Bm1370`), selected at enumeration from the
/// chip-id register. Each method drives `bus` imperatively; host-side
/// policy (PSU voltage, the host UART baud switch, work dispatch, the
/// frequency ramp) stays in the consumer, not here. Chip count and
/// series topology come from the `HashboardDescriptor`.
//
// The default method bodies are the bring-up recipe shared by every
// chip; a chip model varies only through [`Self::descriptor`]. A model
// that genuinely diverges on a step overrides just that method.
#[allow(async_fn_in_trait)]
pub trait ChipProtocol {
    /// Static capability and identity constants for this chip model.
    fn descriptor(&self) -> &'static BmDescriptor;

    /// Bring the chain to a known reset state and drop any assigned
    /// addresses so it can be re-enumerated. Also enables hardware
    /// version rolling here, because that switches the chip's response
    /// frames to their 11-byte form — which the parser and every later
    /// read depend on, so it must happen before the first read.
    async fn reset_chain(&self, bus: &mut impl ChipBus) {
        // Mirror the captured reset order: quiesce, configure misc
        // control, then enable hardware version rolling. Version rolling
        // switches responses to their 11-byte form, which the parser and
        // every later read depend on; sequenced last here it still
        // precedes the first read in `enumerate_chips`.
        bus.write_register(ChipAddress::All, SoftResetControlReg::quiescent())
            .await;
        bus.delay_ms(RESET_GAP_MS).await;
        bus.write_register(ChipAddress::All, MiscCtrlReg::default_bringup_config())
            .await;
        bus.delay_ms(RESET_GAP_MS).await;
        bus.write_register(ChipAddress::All, MidstateCalReg::factory_default())
            .await;
        bus.delay_ms(RESET_GAP_MS).await;
        for _ in 0..CHAIN_INACTIVE_REPEATS {
            bus.send_command(ControlCommand::chain_inactive()).await;
            bus.delay_ms(CHAIN_INACTIVE_GAP_MS).await;
        }
    }

    /// Assign addresses up to the chain cap, then broadcast-read the
    /// chip-id register to discover which chips responded. Returns the
    /// count of responders whose chip-id matches [`Self::descriptor`].
    async fn enumerate_chips(&self, bus: &mut impl ChipBus) -> u8 {
        // Assign addresses up to the chain cap; chips claim them in
        // order, so we never assume a fixed count or spacing.
        for address in 0..MAX_CHAIN_CHIPS {
            // address < MAX_CHAIN_CHIPS (32), well below the 0xFF
            // broadcast address, so the cast is exact and valid.
            #[allow(clippy::cast_possible_truncation)]
            bus.send_command(ControlCommand::set_chip_address(address as u8))
                .await;
        }
        bus.delay_ms(SETTLE_GAP_MS).await;
        // Discover which addresses answer with this chip's id.
        let responders = bus.read_register::<ChipIdReg>(ChipAddress::All).await;
        // `count` is bounded by `MAX_CHAIN_CHIPS`, well within u8.
        #[allow(clippy::cast_possible_truncation)]
        let count = responders
            .iter()
            .filter(|(_, chip_id)| chip_id.chip_id() == self.descriptor().chip_id)
            .count() as u8;
        count
    }

    /// Configure the per-domain output drivers (and, on chains with more
    /// than one chip per domain, UART relaying) for the series topology
    /// from `board`.
    ///
    /// Always broadcasts `AnalogMuxControl::disabled()` and
    /// `IoDriverStrengthCtrlReg::default_drive_strengths()` first. When
    /// `board.num_voltage_domains == 0` or `chips_per_domain <= 1` (which
    /// covers single-chip-per-domain hashboards such as the four-chip
    /// BMM101), no relay programming is emitted. Otherwise, for each
    /// domain the last chip receives a boosted clock-out drive strength
    /// and both the first and last chips are programmed with the
    /// relay-enabled `UartRelayReg`. The gap count scales with how far
    /// the domain sits from the chain head so the relay inserts enough
    /// inter-byte spacing.
    async fn initialize_domains(&self, bus: &mut impl ChipBus, board: &HashboardDescriptor) {
        bus.write_register(ChipAddress::All, AnalogMuxControl::disabled())
            .await;
        bus.write_register(
            ChipAddress::All,
            IoDriverStrengthCtrlReg::default_drive_strengths(),
        )
        .await;

        let num_domains = board.num_voltage_domains;
        if num_domains == 0 {
            return;
        }

        // Chips are assumed to divide evenly across domains. With a
        // remainder, integer division rounds down and the trailing chips
        // fall outside every [first_chip, last_chip] span below, so they
        // receive no relay programming. Every modeled board divides
        // evenly; the assert pins the precondition for any new descriptor.
        debug_assert!(
            board.num_chips.is_multiple_of(num_domains),
            "chips must divide evenly across voltage domains; remainder chips receive no relay programming"
        );
        let chips_per_domain = board.num_chips / num_domains;
        if chips_per_domain <= 1 {
            // Single-chip-per-domain chains need no UART relay; the
            // default drive strengths written above are sufficient.
            return;
        }

        for domain_index in 0..num_domains {
            // Widen to u16 for index arithmetic to avoid overflow on
            // boards with large chip counts. Chip addresses equal chip
            // indices, so the addressing stride is one.
            let domain_index_wide = u16::from(domain_index);
            let chips_per_domain_wide = u16::from(chips_per_domain);
            let num_chips_wide = u16::from(board.num_chips);

            // First and last chip addresses in this domain, bounded by
            // num_chips. Both stay within num_chips - 1 <= u8::MAX.
            #[allow(clippy::cast_possible_truncation)]
            let first_chip = (domain_index_wide * chips_per_domain_wide) as u8;
            #[allow(clippy::cast_possible_truncation)]
            let last_chip = (((domain_index_wide + 1) * chips_per_domain_wide - 1)
                .min(num_chips_wide - 1)) as u8;

            // The gap count scales with the number of domains still
            // downstream of this one, so the chain head reserves the
            // most relay spacing and each domain toward the tail
            // reserves less. The product of two u8 plus RELAY_BASE_GAP
            // stays below u16::MAX.
            #[allow(clippy::cast_possible_truncation)]
            let gap_cnt = (u32::from(chips_per_domain) * u32::from(num_domains - domain_index)
                + RELAY_BASE_GAP) as u16;

            bus.write_register(
                ChipAddress::Chip(last_chip),
                IoDriverStrengthCtrlReg::domain_boundary(),
            )
            .await;

            bus.write_register(
                ChipAddress::Chip(first_chip),
                UartRelayReg::relays_enabled(gap_cnt),
            )
            .await;

            // With an uneven topology the `.min(num_chips - 1)` clamp can
            // pull `last_chip` onto `first_chip`; the guard then avoids a
            // redundant duplicate relay write to the same chip.
            if first_chip != last_chip {
                bus.write_register(
                    ChipAddress::Chip(last_chip),
                    UartRelayReg::relays_enabled(gap_cnt),
                )
                .await;
            }
        }
    }

    /// Configure the per-core clock, delay, and reset settings.
    async fn initialize_cores(&self, bus: &mut impl ChipBus, board: &HashboardDescriptor) {
        bus.write_register(
            ChipAddress::All,
            CoreControlReg::default_hash_clock_config(),
        )
        .await;
        bus.delay_ms(SETTLE_GAP_MS).await;
        bus.write_register(
            ChipAddress::All,
            CoreControlReg::for_core_write(
                CoreRegister::ClockDelayCtrl,
                self.descriptor().broadcast_clock_delay,
            ),
        )
        .await;
        bus.delay_ms(SETTLE_GAP_MS).await;
        for index in 0..board.num_chips {
            let chip = ChipAddress::Chip(index);
            bus.write_register(
                chip,
                SoftResetControlReg::reset_core_domain(DomainSelect::Domain1),
            )
            .await;
            bus.write_register(chip, MiscCtrlReg::default_bringup_config())
                .await;
            bus.write_register(chip, CoreControlReg::default_hash_clock_config())
                .await;
            bus.write_register(chip, CoreControlReg::default_clock_delay_config())
                .await;
            bus.write_register(chip, CoreControlReg::default_core_reg2_config())
                .await;
        }
        self.write_misc_settings(bus).await;
    }

    /// Apply chip-specific register writes that the shared recipe does not
    /// cover, run at the tail of [`Self::initialize_cores`]. The default is
    /// a no-op; a chip model overrides this when its bring-up needs an extra
    /// register the others do not.
    async fn write_misc_settings(&self, _bus: &mut impl ChipBus) {}

    /// Program each chip a disjoint nonce offset via the nonce-offset
    /// register so the chain searches the nonce space without overlap.
    async fn split_nonce_space(&self, bus: &mut impl ChipBus, board: &HashboardDescriptor) {
        let num_chips = board.num_chips;
        if num_chips == 0 {
            return;
        }

        // Divide the 16-bit nonce-offset space into `num_chips` equal
        // slices; the per-step `- 1` reproduces the chain's small
        // per-chip skew. `index * step` stays below the 16-bit space.
        let step = (NONCE_OFFSET_SPACE / u32::from(num_chips)).saturating_sub(1);
        for index in 0..num_chips {
            // `index < num_chips`, so `index * step` fits the 16-bit space.
            #[allow(clippy::cast_possible_truncation)]
            let offset = (u32::from(index) * step) as u16;
            bus.write_register(
                ChipAddress::Chip(index),
                ChipNonceOffsetReg::from_offset(offset),
            )
            .await;
        }
    }

    /// Program the chip side of the fast-UART switch. Run this last in
    /// bring-up: the fast UART is crystal-clocked, so it needs no PLL
    /// configured first. The consumer switches the host UART afterwards
    /// to the rate returned by [`Self::fast_uart_host_rate`].
    async fn enable_fast_uart(&self, bus: &mut impl ChipBus) {
        // The fast UART is crystal-clocked at a single rate; the consumer
        // switches the host controller to match afterwards.
        bus.write_register(ChipAddress::All, BaudRateReg::fast_3mbaud())
            .await;
    }

    /// Host UART rate, in Hz, to pair with the chip register written by
    /// [`Self::enable_fast_uart`]. The default fast UART is crystal-
    /// clocked at a nominal 3.125 Mbaud (bt8d=0), which the host
    /// controllers cannot divide to exactly; they run 3.0 Mbaud, which
    /// the chip tolerates. This hook therefore returns the achievable
    /// host rate, not the chip's nominal one, so the consumer programs
    /// it verbatim. Chips whose serial pair cannot sustain 3 Mbaud
    /// override this to return a slower rate derived from their own
    /// bt8d.
    fn fast_uart_host_rate(&self) -> u32 {
        3_000_000
    }

    /// Program the hash-clock PLL to `freq`, leaving the counting number
    /// untouched. The fast UART is crystal-clocked, so the capture
    /// programs the PLL alone before the baud switch; the counting
    /// number follows on the fast UART via [`Self::set_hcn`].
    async fn set_pll(&self, bus: &mut impl ChipBus, freq: Frequency) -> Result<(), ChipError> {
        let range = self.descriptor().chain.frequency_range;
        let pll = PllParameterReg::from_freq(freq.khz(), range)
            .ok_or(ChipError::UnsupportedFrequency(freq))?;
        let fence = Pll0DividerReg::zeroed();
        // The trailing duplicate covers a PLL write that arrives garbled
        // because the PLL itself shifted mid-reception.
        bus.write_register(ChipAddress::All, fence).await;
        bus.write_register(ChipAddress::All, pll).await;
        bus.write_register(ChipAddress::All, fence).await;
        bus.write_register(ChipAddress::All, pll).await;
        Ok(())
    }

    /// Per-chip core re-arm run immediately after [`Self::set_pll`]. Some
    /// chips latch their core-clock configuration against the running hash
    /// clock, so the core-clock-enable writes only take once the PLL is up;
    /// doing them earlier (against the boot clock) leaves cores idle and
    /// roughly halves the hashrate. The default is a no-op for chips that
    /// configure their cores entirely before the PLL.
    async fn post_pll_rearm(&self, _bus: &mut impl ChipBus) {}

    /// Program the hash-counting number derived from `freq`. The capture
    /// only ever sends it on the fast UART, so bring-up programs it after
    /// the baud switch, never at the boot baud.
    async fn set_hcn(&self, bus: &mut impl ChipBus, freq: Frequency) -> Result<(), ChipError> {
        let range = self.descriptor().chain.frequency_range;
        let hcn = HashCountingNumberReg::from_freq(freq.khz(), range)
            .ok_or(ChipError::UnsupportedFrequency(freq))?;
        bus.write_register(ChipAddress::All, hcn).await;
        Ok(())
    }

    /// Program the counting number and PLL together for one step of the
    /// frequency ramp, where the capture pairs them on the fast UART.
    async fn set_hash_freq(
        &self,
        bus: &mut impl ChipBus,
        freq: Frequency,
    ) -> Result<(), ChipError> {
        self.set_hcn(bus, freq).await?;
        self.set_pll(bus, freq).await?;
        Ok(())
    }
}

/// Per-chip capability constants for the BM1362. `max_chip_voltage`
/// is hashboard-local tuning and safety policy; the 422 mV figure is
/// not a chip-spec value. The `frequency_range` upper bound is a
/// uniform 1000 MHz local tuning-policy envelope; the chip has no hard
/// limit there. `frequency_step` is 6.25 MHz, the chip's natural PLL
/// resolution: at the `(POSTDIV1+1)*(POSTDIV2+1) = 2` divider pair
/// `freq = 6.25 * FBDIV` MHz, so every integer FBDIV step sits on a
/// 6.25 MHz boundary the PLL calculator resolves exactly. The BM1366
/// and BM1370 share this grid.
pub static BM1362: BmDescriptor = BmDescriptor {
    chain: ChipDescriptor {
        asic_model: Chip::Bm1362.asic_model(),
        core_count: 514,
        max_chip_voltage: Chip::Bm1362.max_chip_voltage(),
        frequency_step: Frequency::from_khz(6_250),
        frequency_range: FrequencyRange::new(
            Frequency::from_khz(50_000),
            Frequency::from_khz(1_000_000),
        ),
    },
    chip_id: 0x1362,
    broadcast_clock_delay: 0x34,
    midstate_count: 8,
};

/// Per-chip capability constants for the BM1366. `max_chip_voltage` is
/// the whole-chip ceiling, hashboard-local safety policy rather than a
/// chip-spec value. `frequency_step` is the shared 6.25 MHz grid; the
/// `frequency_range` upper bound is a uniform 1000 MHz local
/// tuning-policy envelope, not a chip limit.
pub static BM1366: BmDescriptor = BmDescriptor {
    chain: ChipDescriptor {
        asic_model: Chip::Bm1366.asic_model(),
        core_count: 894,
        max_chip_voltage: Chip::Bm1366.max_chip_voltage(),
        frequency_step: Frequency::from_khz(6_250),
        frequency_range: FrequencyRange::new(
            Frequency::from_khz(50_000),
            Frequency::from_khz(1_000_000),
        ),
    },
    chip_id: 0x1366,
    broadcast_clock_delay: 0x34,
    midstate_count: 8,
};

/// Per-chip capability constants for the BM1370. `max_chip_voltage` is
/// the whole-chip ceiling, hashboard-local safety policy rather than a
/// chip-spec value. `frequency_step` is the shared 6.25 MHz grid; the
/// `frequency_range` upper bound is a uniform 1000 MHz local
/// tuning-policy envelope, not a chip limit.
pub static BM1370: BmDescriptor = BmDescriptor {
    chain: ChipDescriptor {
        asic_model: Chip::Bm1370.asic_model(),
        core_count: 2040,
        max_chip_voltage: Chip::Bm1370.max_chip_voltage(),
        frequency_step: Frequency::from_khz(6_250),
        frequency_range: FrequencyRange::new(
            Frequency::from_khz(250_000),
            Frequency::from_khz(1_000_000),
        ),
    },
    chip_id: 0x1370,
    broadcast_clock_delay: 0x34,
    midstate_count: 16,
};

// The driver divides by `midstate_count` unguarded — `job_slots` and the
// work-id decode rely on a nonzero stride that divides the echoed byte's
// range exactly. Enforce both invariants where the descriptors are
// defined, at compile time, so a future descriptor cannot reintroduce
// the divide-by-zero class the empty-chain guard fixed at runtime.
const _: () = {
    assert!(BM1362.midstate_count > 0);
    assert!(BM1366.midstate_count > 0);
    assert!(BM1370.midstate_count > 0);
    assert!(
        (Chip::Bm1362.max_work_id() as usize + 1).is_multiple_of(BM1362.midstate_count as usize)
    );
    assert!(
        (Chip::Bm1366.max_work_id() as usize + 1).is_multiple_of(BM1366.midstate_count as usize)
    );
    assert!(
        (Chip::Bm1370.max_work_id() as usize + 1).is_multiple_of(BM1370.midstate_count as usize)
    );
};

/// The chip model a hashboard is populated with. Each firmware injects
/// its `config::CHIP` constant into `Bm13xxDriver::new`; the hashboard
/// descriptor carries only the neutral `ChipDescriptor`. This enum
/// dispatches the shared bring-up recipe to the concrete per-chip
/// implementation: `ChipProtocol` has async methods and is not
/// dyn-compatible, so an enum is the dispatch mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Chip {
    /// BM1362 ASIC variant.
    Bm1362,
    /// BM1366 ASIC variant.
    Bm1366,
    /// BM1370 ASIC variant.
    Bm1370,
}

impl Chip {
    /// Model string reported to the REST API and dashboard. `const` so the
    /// static `BmDescriptor` initialisers below can evaluate it for the
    /// `ChipDescriptor::asic_model` field, which `ubos-config`'s
    /// `mining_info` reads.
    #[must_use]
    pub const fn asic_model(self) -> &'static str {
        match self {
            Chip::Bm1362 => "BM1362",
            Chip::Bm1366 => "BM1366",
            Chip::Bm1370 => "BM1370",
        }
    }

    /// Per-chip voltage ceiling: board-local safety policy, not a chip-spec
    /// value. `const` so the board descriptor's rail-voltage math and the
    /// tuning bounds evaluate at compile time. Single source of truth: the
    /// static `BmDescriptor`s below initialise their chain descriptor's
    /// `max_chip_voltage` from here.
    #[must_use]
    pub const fn max_chip_voltage(self) -> Voltage {
        match self {
            Chip::Bm1362 => Voltage::from_mv(422),
            Chip::Bm1366 => Voltage::from_mv(1382),
            Chip::Bm1370 => Voltage::from_mv(1250),
        }
    }

    /// Recover the job's work-id from byte 7 of a work response.
    ///
    /// The BM1370 packs the id into the high nibble as
    /// `(work_id << 1) | small_core_id`, so `(byte & 0xf0) >> 1` recovers
    /// it; its `midstate_count` of 16 keeps the low 4 bits of the work-id
    /// byte zero, so the shift loses nothing. The BM1362/BM1366 carry the
    /// work-id directly in byte 7.
    #[must_use]
    pub const fn decode_response_work_id(self, byte: u8) -> u8 {
        match self {
            Chip::Bm1370 => (byte & 0xf0) >> 1,
            Chip::Bm1362 | Chip::Bm1366 => byte,
        }
    }

    /// Largest dispatched work-id the chip's response byte can carry.
    ///
    /// The BM1370 encodes the work-id in the high nibble as
    /// `(work_id << 1) | small_core_id`; an id of 128 or above overflows
    /// the byte and decodes to the wrong slot. The dispatched id must
    /// therefore stay within `0..=127`. Chips that carry the id directly
    /// (BM1362, BM1366) use the full u8 range.
    #[must_use]
    pub const fn max_work_id(self) -> u8 {
        match self {
            Chip::Bm1370 => 127,
            Chip::Bm1362 | Chip::Bm1366 => u8::MAX,
        }
    }
}

impl ChipProtocol for Chip {
    fn descriptor(&self) -> &'static BmDescriptor {
        match self {
            Chip::Bm1362 => &BM1362,
            Chip::Bm1366 => &BM1366,
            Chip::Bm1370 => &BM1370,
        }
    }

    // Forward every method a concrete chip overrides. The shared
    // default-method steps are inherited and key off `descriptor()`;
    // only explicitly-overridden steps must be dispatched here. When a
    // chip overrides a new step, add the matching forward below.
    async fn write_misc_settings(&self, bus: &mut impl ChipBus) {
        match self {
            Chip::Bm1362 => Bm1362.write_misc_settings(bus).await,
            Chip::Bm1366 => Bm1366.write_misc_settings(bus).await,
            Chip::Bm1370 => Bm1370.write_misc_settings(bus).await,
        }
    }

    async fn enable_fast_uart(&self, bus: &mut impl ChipBus) {
        match self {
            Chip::Bm1362 => Bm1362.enable_fast_uart(bus).await,
            Chip::Bm1366 => Bm1366.enable_fast_uart(bus).await,
            Chip::Bm1370 => Bm1370.enable_fast_uart(bus).await,
        }
    }

    fn fast_uart_host_rate(&self) -> u32 {
        match self {
            Chip::Bm1362 => Bm1362.fast_uart_host_rate(),
            Chip::Bm1366 => Bm1366.fast_uart_host_rate(),
            Chip::Bm1370 => Bm1370.fast_uart_host_rate(),
        }
    }

    async fn post_pll_rearm(&self, bus: &mut impl ChipBus) {
        match self {
            Chip::Bm1362 => Bm1362.post_pll_rearm(bus).await,
            Chip::Bm1366 => Bm1366.post_pll_rearm(bus).await,
            Chip::Bm1370 => Bm1370.post_pll_rearm(bus).await,
        }
    }
}

/// BM1362 bring-up recipe. A single internal voltage domain; the
/// BMM101 board stacks several chips in series, addressed `0x00..`.
pub struct Bm1362;

impl ChipProtocol for Bm1362 {
    fn descriptor(&self) -> &'static BmDescriptor {
        &BM1362
    }
}

// ---------------------------------------------------------------------------
// BM1366
// ---------------------------------------------------------------------------

/// BM1366 bring-up recipe. The sequence is shared with BM1362 through
/// the common helpers; the chip differs only in its `BmDescriptor`
/// (higher core voltage and core count). Chains with more than one chip
/// per domain additionally program the UART relay, driven entirely by
/// the `HashboardDescriptor` topology rather than by anything
/// chip-specific.
pub struct Bm1366;

impl ChipProtocol for Bm1366 {
    fn descriptor(&self) -> &'static BmDescriptor {
        &BM1366
    }
}

// ---------------------------------------------------------------------------
// BM1370
// ---------------------------------------------------------------------------

/// BM1370 bring-up recipe. Shares the recipe with BM1362/BM1366 through
/// the default methods, differing only in its `BmDescriptor` and in one
/// extra register: after core configuration it programs the `MiscSettings`
/// register (0xb9), which the other models do not require.
pub struct Bm1370;

impl ChipProtocol for Bm1370 {
    fn descriptor(&self) -> &'static BmDescriptor {
        &BM1370
    }

    async fn write_misc_settings(&self, bus: &mut impl ChipBus) {
        // Repeated so a single lost frame still leaves it programmed,
        // matching the chain-inactive and reset-config write pattern.
        for _ in 0..MISC_SETTINGS_REPEATS {
            bus.write_register(ChipAddress::All, MiscSettingsReg::bm1370_bringup())
                .await;
        }
    }

    async fn enable_fast_uart(&self, bus: &mut impl ChipBus) {
        // The BM1370 chip serial pair cannot sustain 3 Mbaud; use the
        // SI-safe rate (520 833 Hz default, bt8d=5) instead. The host
        // switches to the matching rate via fast_uart_host_rate().
        bus.write_register(
            ChipAddress::All,
            BaudRateReg::fast_3mbaud().with_bt8d(super::baud::BM1370_FAST_UART_BT8D),
        )
        .await;
    }

    fn fast_uart_host_rate(&self) -> u32 {
        super::baud::BM1370_FAST_UART_HOST_HZ
    }

    async fn post_pll_rearm(&self, bus: &mut impl ChipBus) {
        // The S21 Pro capture re-arms the cores against the running hash
        // clock after the PLL is up: a domain soft-reset, MiscCtrl, then the
        // three CoreControl writes whose last value enables the core clocks —
        // without this the cores stay idle and the hashrate halves. The tail
        // re-asserts the MiscSettings bring-up preset, enables the on-die
        // thermal-diode mux (else the board runs die-temp blind), and sets one
        // more CoreControl. The capture issues these per chip; broadcast is
        // tried here since the same registers are broadcast-written pre-PLL.
        //
        // Each write goes through its typed register. Where the value matches
        // a named preset it uses that; the remaining opaque payloads, whose
        // individual fields are not yet decoded, are loaded byte-exact to the
        // capture via `from_bits`.
        bus.write_register(
            ChipAddress::All,
            SoftResetControlReg::new()
                .with_b7_4(0xf)
                .with_b8(true)
                .with_b23_16(0x07),
        )
        .await;
        bus.write_register(
            ChipAddress::All,
            MiscCtrlReg::default_bringup_config().with_unknown_bits_31_28(0xf),
        )
        .await;
        bus.write_register(
            ChipAddress::All,
            CoreControlReg::for_core_write(CoreRegister::CoreRegB, 0x00),
        )
        .await;
        bus.write_register(
            ChipAddress::All,
            CoreControlReg::for_core_write(CoreRegister::ClockDelayCtrl, 0x0c),
        )
        .await;
        bus.write_register(ChipAddress::All, CoreControlReg::default_core_reg2_config())
            .await;
        bus.write_register(ChipAddress::All, MiscSettingsReg::bm1370_bringup())
            .await;
        // ana_mux_sel = 2 routes the on-die thermal diode to the sensor.
        bus.write_register(
            ChipAddress::All,
            AnalogMuxControl::new().with_ana_mux_sel(0x02),
        )
        .await;
        bus.write_register(ChipAddress::All, MiscSettingsReg::bm1370_bringup())
            .await;
        bus.write_register(
            ChipAddress::All,
            CoreControlReg::for_core_write(CoreRegister::Unknown(0x0d), 0xee),
        )
        .await;
    }
}
