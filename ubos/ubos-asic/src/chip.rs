//! Per-chip bring-up over an abstract bus (sans-IO).
//!
//! [`ChipProtocol`] is the per-chip bring-up recipe: one implementation
//! per chip model, expressed as plain imperative async steps that write
//! registers, send control commands, and wait settle delays. It owns no
//! UART and performs no direct I/O; it drives [`ChipBus`], which the
//! consumer (`ubos-mining`) implements over the real serial link. This
//! keeps the bring-up logic in this pure crate and unit-testable against
//! a mock bus. See `.claude/rules/rust.md` for the sans-IO rule.

use heapless::Vec;
use ubos_core::units::{Frequency, FrequencyRange, Voltage};

use crate::{
    AnalogMuxControl, BaudRateReg, BoardDescriptor, ChipAddress, ChipIdReg, ChipNonceOffsetReg,
    ControlCommand, CoreControlReg, CoreRegister, DomainSelect, HashCountingNumberReg,
    IoDriverStrengthCtrlReg, MidstateCalReg, MiscCtrlReg, MiscSettingsReg, Pll0DividerReg,
    PllParameterReg, Register, SoftResetControlReg, UartRelayReg,
};

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

/// Failure surface of a [`ChipProtocol`] step. Reads return an empty
/// list rather than an error, so the only fallible step is programming
/// a frequency outside the chip's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipError {
    /// The requested frequency is outside the chip's range.
    UnsupportedFrequency(Frequency),
}

// Hand-written so the error can carry a `Frequency` without `ubos-core`
// taking a defmt dependency; the frequency is logged in kHz.
#[cfg(feature = "defmt")]
impl defmt::Format for ChipError {
    fn format(&self, fmt: defmt::Formatter) {
        match self {
            ChipError::UnsupportedFrequency(freq) => {
                defmt::write!(fmt, "UnsupportedFrequency({} kHz)", freq.khz());
            }
        }
    }
}

/// The chip-side I/O a [`ChipProtocol`] drives. Implemented by the
/// consumer, which owns the UART and a timer. Register writes and
/// control commands are fire-and-forget; only reads can fail.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu` and `ubos_mining`'s `MinerControl`.
// Send bounds are not required on this single-task embedded target.
#[allow(async_fn_in_trait)]
pub trait ChipBus {
    /// Write a raw `value` to register `reg` on `addr` (a single chip or
    /// the broadcast address). The primitive the consumer implements;
    /// recipes use the typed [`Self::write_register`] instead.
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

/// Per-chip bring-up recipe. One implementation per chip model
/// (`Bm1362` / `Bm1366` / `Bm1370`), selected at enumeration from the
/// chip-id register. Each method drives `bus` imperatively; host-side
/// policy (PSU voltage, the host UART baud switch, work dispatch, the
/// frequency ramp) stays in the consumer, not here. Chip count and
/// series topology come from the `BoardDescriptor`.
//
// The default method bodies are the bring-up recipe shared by every
// chip; a chip model varies only through [`Self::descriptor`]. A model
// that genuinely diverges on a step overrides just that method.
#[allow(async_fn_in_trait)]
pub trait ChipProtocol {
    /// Static capability and identity constants for this chip model.
    fn descriptor(&self) -> &'static ChipDescriptor;

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
    /// covers single-chip-per-domain boards such as the four-chip BMM100),
    /// no relay programming is emitted. Otherwise, for each domain the
    /// last chip receives a boosted clock-out drive strength and both the
    /// first and last chips are programmed with the relay-enabled
    /// `UartRelayReg`. The gap count scales with how far the domain sits
    /// from the chain head so the relay inserts enough inter-byte spacing.
    async fn initialize_domains(&self, bus: &mut impl ChipBus, board: &BoardDescriptor) {
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
    async fn initialize_cores(&self, bus: &mut impl ChipBus, board: &BoardDescriptor) {
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
    async fn split_nonce_space(&self, bus: &mut impl ChipBus, board: &BoardDescriptor) {
        // Divide the 16-bit nonce-offset space into `num_chips` equal
        // slices; the per-step `- 1` reproduces the chain's small
        // per-chip skew. `index * step` stays below the 16-bit space.
        let step = (NONCE_OFFSET_SPACE / u32::from(board.num_chips)).saturating_sub(1);
        for index in 0..board.num_chips {
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
    /// configured first. The consumer switches the host UART afterwards.
    async fn enable_fast_uart(&self, bus: &mut impl ChipBus) {
        // The fast UART is crystal-clocked at a single rate; the consumer
        // switches the host controller to match afterwards.
        bus.write_register(ChipAddress::All, BaudRateReg::fast_3mbaud())
            .await;
    }

    /// Program the hash-clock PLL to `freq`, leaving the counting number
    /// untouched. The fast UART is crystal-clocked, so the capture
    /// programs the PLL alone before the baud switch; the counting
    /// number follows on the fast UART via [`Self::set_hcn`].
    async fn set_pll(&self, bus: &mut impl ChipBus, freq: Frequency) -> Result<(), ChipError> {
        let range = self.descriptor().frequency_range;
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

    /// Program the hash-counting number derived from `freq`. The capture
    /// only ever sends it on the fast UART, so bring-up programs it after
    /// the baud switch, never at the boot baud.
    async fn set_hcn(&self, bus: &mut impl ChipBus, freq: Frequency) -> Result<(), ChipError> {
        let range = self.descriptor().frequency_range;
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

/// Capability bounds for a single ASIC, consumed by the tuner.
#[derive(Clone, Copy)]
pub struct ChipDescriptor {
    /// Model identifier the chip reports in `ChipIdReg.chip_id`
    /// (`0x1362`, `0x1366`, `0x1370`). Enumeration matches the value
    /// read off the chain against this to select the chip.
    pub chip_id: u16,
    /// Total hashing cores on this chip model. Identity and capability
    /// metadata; not a register value.
    pub core_count: u32,
    /// Maximum voltage a single whole chip tolerates. Board-local safety
    /// policy, not a chip-spec value. The board-rail ceiling consumed by
    /// the tuner is this value scaled by the hashboard's
    /// `num_voltage_domains`.
    pub max_chip_voltage: Voltage,
    /// PLL hash-clock resolution: the frequency grid every programmed
    /// operating point lands on. 6.25 MHz is the BM1362's natural
    /// resolution. At the `(POSTDIV1+1)*(POSTDIV2+1) = 2` divider pair
    /// `freq = 6.25 * FBDIV` MHz, so every integer FBDIV step sits on a
    /// 6.25 MHz boundary the PLL calculator resolves exactly. The tuner
    /// ramps on and floors targets to this grid so the requested
    /// frequency equals the realized hash clock.
    pub frequency_step: Frequency,
    /// Inclusive hash-clock frequency envelope the chip operates within.
    /// The tuner ramps inside it and the register calculators reject
    /// targets outside it.
    pub frequency_range: FrequencyRange,
    /// Broadcast `ClockDelayCtrl` value loaded during core bring-up,
    /// before the per-chip pass writes the final clock delay.
    pub broadcast_clock_delay: u8,
    /// Number of distinct work-id values the chip emits per dispatched
    /// work item. The work frame sends a single header (multiversion 1);
    /// the chip tags each returned solution with a variant index in the
    /// low bits of the echoed work-id. The host strides work-ids by this
    /// count and recovers the originating job by dividing a returned
    /// work-id by it. Eight for the BM1362 and BM1366, sixteen for the
    /// BM1370.
    pub midstate_count: u8,
}

/// Per-chip capability constants for the BM1362. `max_chip_voltage`
/// is board-local tuning and safety policy; the 422 mV figure is not a
/// chip-spec value. The `frequency_range` upper bound is a uniform
/// 1000 MHz local tuning-policy envelope; the chip has no hard limit
/// there.
pub static BM1362: ChipDescriptor = ChipDescriptor {
    chip_id: 0x1362,
    core_count: 514,
    max_chip_voltage: Voltage::from_mv(422),
    frequency_step: Frequency::from_khz(6_250),
    frequency_range: FrequencyRange::new(
        Frequency::from_khz(50_000),
        Frequency::from_khz(1_000_000),
    ),
    broadcast_clock_delay: 0x34,
    midstate_count: 8,
};

/// Per-chip capability constants for the BM1366. `max_chip_voltage` is
/// the whole-chip ceiling, board-local safety policy rather than a
/// chip-spec value. `frequency_step` is the shared 6.25 MHz grid; the
/// `frequency_range` upper bound is a uniform 1000 MHz local
/// tuning-policy envelope, not a chip limit.
pub static BM1366: ChipDescriptor = ChipDescriptor {
    chip_id: 0x1366,
    core_count: 894,
    max_chip_voltage: Voltage::from_mv(1382),
    frequency_step: Frequency::from_khz(6_250),
    frequency_range: FrequencyRange::new(
        Frequency::from_khz(50_000),
        Frequency::from_khz(1_000_000),
    ),
    broadcast_clock_delay: 0x34,
    midstate_count: 8,
};

/// Per-chip capability constants for the BM1370. `max_chip_voltage` is
/// the whole-chip ceiling, board-local safety policy rather than a
/// chip-spec value. `frequency_step` is the shared 6.25 MHz grid; the
/// `frequency_range` upper bound is a uniform 1000 MHz local
/// tuning-policy envelope, not a chip limit.
pub static BM1370: ChipDescriptor = ChipDescriptor {
    chip_id: 0x1370,
    core_count: 2040,
    max_chip_voltage: Voltage::from_mv(1250),
    frequency_step: Frequency::from_khz(6_250),
    frequency_range: FrequencyRange::new(
        Frequency::from_khz(250_000),
        Frequency::from_khz(1_000_000),
    ),
    broadcast_clock_delay: 0x34,
    midstate_count: 16,
};

/// The chip model a board is populated with. Selected statically from
/// the board descriptor; it dispatches the shared bring-up recipe to the
/// concrete per-chip implementation. `ChipProtocol` has async methods and
/// is not dyn-compatible, so this enum is the dispatch mechanism.
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

impl ChipProtocol for Chip {
    fn descriptor(&self) -> &'static ChipDescriptor {
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
}

/// BM1362 bring-up recipe. A single internal voltage domain; the
/// BMM100 board stacks several chips in series, addressed `0x00..`.
pub struct Bm1362;

impl ChipProtocol for Bm1362 {
    fn descriptor(&self) -> &'static ChipDescriptor {
        &BM1362
    }
}

// ---------------------------------------------------------------------------
// BM1366
// ---------------------------------------------------------------------------

/// BM1366 bring-up recipe. The sequence is shared with BM1362 through
/// the common helpers; the chip differs only in its `ChipDescriptor`
/// (higher core voltage and core count). Chains with more than one chip
/// per domain additionally program the UART relay, driven entirely by
/// the `BoardDescriptor` topology rather than by anything chip-specific.
pub struct Bm1366;

impl ChipProtocol for Bm1366 {
    fn descriptor(&self) -> &'static ChipDescriptor {
        &BM1366
    }
}

// ---------------------------------------------------------------------------
// BM1370
// ---------------------------------------------------------------------------

/// BM1370 bring-up recipe. Shares the recipe with BM1362/BM1366 through
/// the default methods, differing only in its `ChipDescriptor` and in one
/// extra register: after core configuration it programs the `MiscSettings`
/// register (0xb9), which the other models do not require.
pub struct Bm1370;

impl ChipProtocol for Bm1370 {
    fn descriptor(&self) -> &'static ChipDescriptor {
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
}
