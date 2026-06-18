//! BM1362 typed register layouts.
//!
//! Each register implements [`Register`] so the serial actor can drive
//! generic `write_register<R>` / `read_register<R>` paths that pack
//! and unpack the wire payload through the storage `u32`.
//!
//! See `docs/REGISTERS.md` for a per-register reference.

use bitfield_struct::bitfield;
use ubos_core::units::{Frequency, Voltage};

use crate::chip::{Chip, ChipProtocol};

#[cfg(test)]
#[path = "tests/registers.rs"]
mod tests;

/// Identifies a chip register by its protocol-level address and pins
/// its wire payload to a big-endian 4-byte payload.
pub trait Register: Sized {
    /// Register address as the chip sees it on the wire.
    const REG_NUM: u8;
    /// Decode the wire payload (MSB-first on the bus) into the typed
    /// register value.
    fn from_be_bytes(bytes: [u8; 4]) -> Self;
    /// Encode the typed register value into the wire payload (MSB-first
    /// on the bus).
    fn to_be_bytes(&self) -> [u8; 4];
}

/// VIL control command code (3-bit field in the cmd byte). Register
/// reads and writes use `SetConfig`/`GetStatus` codes that have their
/// own builders; this enum covers the register-less control commands
/// the host issues during chain bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum ControlCode {
    /// Assign an address to the next chip on the chain that has not
    /// claimed one yet.
    SetChipAddress = 0,
    /// Tell every chip on the chain to drop its assigned address.
    ChainInactive = 3,
}

impl ControlCode {
    /// Numeric code packed into the low bits of the cmd byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Register-less VIL control command. Carries the protocol code,
/// the broadcast flag, and the hw-addr the chip-side decoder
/// consults. Construct via [`Self::chain_inactive`] or
/// [`Self::set_chip_address`] and hand to the wire encoder; the
/// encoder is the only place that knows the preamble + CRC framing.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ControlCommand {
    code: ControlCode,
    broadcast: bool,
    hw_addr: u8,
}

impl ControlCommand {
    /// Broadcast command that tells every chip on the chain to drop
    /// its assigned address. Issued three times at bring-up to
    /// tolerate any single-frame loss before address re-assignment
    /// begins.
    #[must_use]
    pub const fn chain_inactive() -> Self {
        Self {
            code: ControlCode::ChainInactive,
            broadcast: true,
            hw_addr: 0x00,
        }
    }

    /// Unicast command that assigns `hw_addr` to the first chip on
    /// the chain without a claimed address. Broadcasting would set
    /// every chip to the same address; the caller walks one
    /// `set_chip_address` per chip slot instead.
    #[must_use]
    pub const fn set_chip_address(hw_addr: u8) -> Self {
        Self {
            code: ControlCode::SetChipAddress,
            broadcast: false,
            hw_addr,
        }
    }

    /// Protocol code carried in the low bits of the cmd byte.
    #[must_use]
    pub const fn code(&self) -> ControlCode {
        self.code
    }

    /// `true` if the cmd byte must set the broadcast bit.
    #[must_use]
    pub const fn is_broadcast(&self) -> bool {
        self.broadcast
    }

    /// hw-addr the chip-side decoder consults; `0x00` for broadcast.
    #[must_use]
    pub const fn hw_addr(&self) -> u8 {
        self.hw_addr
    }
}

/// Chip identity register (`0x00`).
///
/// Read-only; the chip reports its model identifier, hash-core count,
/// and the on-chain address assigned during enumeration.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ChipIdReg {
    /// On-chain address assigned by the host during enumeration.
    pub address: u8,
    /// Number of hashing cores reported by the chip.
    pub core_num: u8,
    /// Model identifier. `0x1362` for BM1362.
    pub chip_id: u16,
}

impl Register for ChipIdReg {
    const REG_NUM: u8 = 0x00;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// PLL0 parameter register (`0x08`).
///
/// Programs the integer-N PLL that derives the hash clock from the
/// 25 MHz crystal, with the reference divider pinned to 2:
/// `freq_mhz = 25 * fbdiv / 2 / ((postdiv1_minus_1 + 1) * (postdiv2_minus_1 + 1))`.
///
/// The same layout repeats at addresses `0x60`, `0x64`, `0x68` for
/// PLL1 / PLL2 / PLL3. Bits 13:8 are the reference divider; the BM1362
/// calculator pins that field to [`PLL_REFDIV_VALUE`].
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PllParameterReg {
    /// Post-divider 2, encoded as `actual_pd2 - 1` in three bits.
    /// Valid stored range `0..=5`; actual divisor is `stored + 1`.
    #[bits(3)]
    pub postdiv2_minus_1: u8,
    #[bits(1)]
    __: u8,
    /// Post-divider 1, encoded as `actual_pd1 - 1` in three bits.
    /// Valid stored range `1..=6`; actual divisor is `stored + 1`.
    #[bits(3)]
    pub postdiv1_minus_1: u8,
    #[bits(1)]
    __: u8,
    /// Reference divider at bits 13:8; the BM1362 frequency calculator
    /// pins this to [`PLL_REFDIV_VALUE`].
    pub refdiv: u8,
    /// Forward divider; the VCO output is `25 MHz * fbdiv / 2`.
    pub fbdiv: u8,
    #[bits(4)]
    __: u8,
    /// VCO high-frequency band select (bit 28). [`Self::from_freq`]
    /// sets it when the VCO runs at or above
    /// [`PLL_VCO_HIGH_THRESHOLD_HZ`], derived from the chosen FBDIV.
    pub vco_high: bool,
    #[bits(1)]
    __: u8,
    /// PLL enable bit. Always `true` for an active hash clock.
    pub pll_en: bool,
    /// PLL lock-detector output. Read-only; host writes ignore this bit.
    pub locked: bool,
}

impl Register for PllParameterReg {
    const REG_NUM: u8 = 0x08;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// PLL reference-divider value written by the BM1362 frequency calculator.
pub const PLL_REFDIV_VALUE: u8 = 0x02;

/// VCO frequency at or above which [`PllParameterReg::vco_high`] is
/// set. This 2400 MHz value is the band-select edge the calculator
/// keys on.
pub const PLL_VCO_HIGH_THRESHOLD_HZ: u64 = 2_400_000_000;

/// PLL1 parameter register (`0x60`). PLL1 drives the UART clock on
/// BM1366; the payload is the [`PllParameterReg`] layout written to
/// this instance address.
pub struct Pll1ParameterReg(pub PllParameterReg);

impl Register for Pll1ParameterReg {
    const REG_NUM: u8 = 0x60;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self(PllParameterReg::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// PLL3 parameter register (`0x68`). PLL3 drives the UART clock on
/// BM1370; the payload is the [`PllParameterReg`] layout written to
/// this instance address.
pub struct Pll3ParameterReg(pub PllParameterReg);

impl Register for Pll3ParameterReg {
    const REG_NUM: u8 = 0x68;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self(PllParameterReg::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// Per-chip nonce-offset register (`0x0c`).
///
/// Splits the 32-bit nonce space across the chain so each chip
/// searches a disjoint sub-range. With [`Self::cnov`] set, the chip
/// derives its initial nonce from [`Self::offset`]; cleared, it falls
/// back to its on-chain address. The exact offset-to-nonce mapping is
/// chip-internal and not modeled here.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ChipNonceOffsetReg {
    /// Per-chip nonce offset selected as the chip's initial nonce when
    /// [`Self::cnov`] is set.
    pub offset: u16,
    #[bits(15)]
    __: u16,
    /// Chip-nonce-offset valid. `true` selects the offset-derived
    /// initial nonce; `false` reverts to the address-derived default.
    pub cnov: bool,
}

impl ChipNonceOffsetReg {
    /// Builds the register with [`Self::cnov`] set and the given
    /// 16-bit per-chip offset.
    #[must_use]
    pub const fn from_offset(offset: u16) -> Self {
        Self::new().with_cnov(true).with_offset(offset)
    }
}

impl Register for ChipNonceOffsetReg {
    const REG_NUM: u8 = 0x0c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Hash-counting-number register (`0x10`).
///
/// Opaque 32-bit hash-counting-number payload. The local calculator
/// derives the payload inversely from target hash-clock frequency via
/// [`Self::from_freq`].
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct HashCountingNumberReg {
    /// Hash-counting number; opaque 32-bit value.
    pub hcn: u32,
}

impl HashCountingNumberReg {
    /// Returns the hash-counting-number for `target_khz`, or `None`
    /// outside the passed `range`. The value scales inversely with the
    /// target clock:
    ///
    /// `hcn = HCN_NUMERATOR_KHZ / freq_khz`
    ///
    /// with [`HCN_NUMERATOR_KHZ`] = `5 * 2^24 * 1000` and integer
    /// (floor) division performed in `u64` since the numerator
    /// exceeds `u32::MAX`.
    #[must_use]
    pub const fn from_freq(target_khz: u32, range: FrequencyRange) -> Option<Self> {
        if target_khz < range.min().khz() || target_khz > range.max().khz() {
            return None;
        }
        // The division result is bounded by
        // HCN_NUMERATOR_KHZ / range.min().khz() = 83_886_080_000 / 50_000
        // = 1_677_721, well inside u32. The truncation cannot lose
        // bits within the calculator's safe range.
        #[allow(clippy::cast_possible_truncation)]
        let hcn = (HCN_NUMERATOR_KHZ / target_khz as u64) as u32;
        Some(Self::new().with_hcn(hcn))
    }
}

impl Register for HashCountingNumberReg {
    const REG_NUM: u8 = 0x10;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Numerator of the local hash-counting-number formula in kHz units,
/// equal to `5 * 2^24 * 1000 = 83_886_080_000`. The HCN scales
/// inversely with hash-clock frequency; this exact `5 * 2^24` per-MHz
/// numerator is a local derivation. The factor of 1000 scales the
/// per-MHz form to kHz so the formula preserves precision for 6.25 MHz
/// PLL steps (kHz integer math avoids the truncation a floor-MHz
/// divisor would impose).
pub const HCN_NUMERATOR_KHZ: u64 = (5u64 << 24) * 1000;

/// Ticket-mask register (`0x14`).
///
/// Configures the chip-side share filter: the chip only returns a
/// nonce when its hash matches the mask, cutting the host's response
/// load by the configured factor. The wire encoding is
/// `(difficulty - 1).reverse_bits().swap_bytes()`. Difficulty must
/// be a power of two so the mask is contiguous high bits. The same
/// encoding repeats at address `0x38` for the second-stage filter.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TicketMaskReg {
    /// Raw mask payload as the chip sees it. Use [`Self::from_difficulty`]
    /// to derive the encoded value from a power-of-two share difficulty.
    pub ticket_mask: u32,
}

impl TicketMaskReg {
    /// Builds the register from a chip-side share difficulty. Returns
    /// `None` for zero or non-power-of-two values; the chip's mask is
    /// a contiguous-high-bits pattern so only powers of two map cleanly.
    #[must_use]
    pub const fn from_difficulty(difficulty: u32) -> Option<Self> {
        if difficulty == 0 || !difficulty.is_power_of_two() {
            return None;
        }
        Some(Self::new().with_ticket_mask((difficulty - 1).reverse_bits().swap_bytes()))
    }
}

impl Register for TicketMaskReg {
    const REG_NUM: u8 = 0x14;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Misc control register (`0x18`).
///
/// Carries baud-rate divisor, pin-function selectors, and other
/// chip-level mode bits. [`MISC_CTRL_DEFAULT_BRINGUP`] captures the
/// bring-up value the chip is programmed with.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MiscCtrlReg {
    #[bits(4)]
    pub unknown_bits_3_0: u8,
    /// TFS pin function selector.
    #[bits(2)]
    pub tfs: u8,
    pub reserved_bit_6: bool,
    pub unknown_bit_7: bool,
    /// Baud-rate divisor; max 26, encodes `OSC/8/baud - 1` at 25 MHz
    /// oscillator. Only consulted on the low-speed UART path.
    #[bits(5)]
    pub baud_div: u8,
    pub unknown_bit_13: bool,
    /// RFS pin function selector.
    #[bits(1)]
    pub rfs: u8,
    pub unknown_bit_15: bool,
    /// Opaque bit at position 16. The V2 register layout groups bits
    /// 19:16 as a raw `B19_16` field.
    pub unknown_bit_16: bool,
    #[bits(5)]
    pub unknown_bits_21_17: u8,
    pub unknown_bit_22: bool,
    #[bits(5)]
    pub unknown_bits_27_23: u8,
    #[bits(4)]
    pub unknown_bits_31_28: u8,
}

impl MiscCtrlReg {
    /// Baseline value the chip is programmed with during bring-up,
    /// before the host raises the line rate. Encodes to
    /// [`MISC_CTRL_DEFAULT_BRINGUP`].
    #[must_use]
    pub const fn default_bringup_config() -> Self {
        Self::from_bits(MISC_CTRL_DEFAULT_BRINGUP)
    }
}

impl Register for MiscCtrlReg {
    const REG_NUM: u8 = 0x18;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// `MiscCtrlReg` baseline value written to every chip during bring-up
/// before the line-rate switch.
pub const MISC_CTRL_DEFAULT_BRINGUP: u32 = 0xb000_c100;

/// Fast-UART configuration register (`0x28`).
///
/// Programs the baud-rate divisor (`bt8d`) and clock-source selector
/// bits for the high-speed UART path:
/// `baud = uart_clk / ((bt8d + 1) * 8)`. Bits without a known field
/// name are preserved as raw groups.
/// [`BAUDRATE_FAST_3MBAUD`] captures the current fast-path preset.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct BaudRateReg {
    #[bits(4)]
    pub raw_bits_3_0: u8,
    pub raw_bit_4: bool,
    #[bits(2)]
    pub raw_bits_6_5: u8,
    pub raw_bit_7: bool,
    /// Baud-tick-8 divider. Encodes line rate as
    /// `baud = uart_clk / ((bt8d + 1) * 8)`.
    pub bt8d: u8,
    pub raw_bit_16: bool,
    pub raw_bit_17: bool,
    #[bits(2)]
    pub raw_bits_19_18: u8,
    /// PLL1 divider field at bits 23:20.
    #[bits(4)]
    pub pll1_div4: u8,
    #[bits(2)]
    pub raw_bits_25_24: u8,
    /// UART baud clock source (`BCK_SEL`).
    #[bits(1)]
    pub bclk_sel: u8,
    pub raw_bit_27: bool,
    /// Raw bit B28.
    pub b28: bool,
    pub raw_bit_29: bool,
    pub raw_bit_30: bool,
    pub raw_bit_31: bool,
}

impl BaudRateReg {
    /// Fast-UART preset used by the current bring-up path. Encodes
    /// byte-exact to [`BAUDRATE_FAST_3MBAUD`]; load this register and
    /// then switch the host serial controller to match.
    #[must_use]
    pub const fn fast_3mbaud() -> Self {
        Self::from_bits(BAUDRATE_FAST_3MBAUD)
    }
}

impl Register for BaudRateReg {
    const REG_NUM: u8 = 0x28;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// `BaudRateReg` value used when switching to the fast UART. With
/// `bt8d=0` and the crystal clock source (`bclk_sel=0`), the chip's
/// actual line rate is `25 MHz / 8 = 3.125 Mbaud`. The host UART is
/// programmed to a nominal 3 Mbaud, which the chip tolerates.
pub const BAUDRATE_FAST_3MBAUD: u32 = 0x1130_0000;

/// UART relay register (`0x2c`).
///
/// Configures per-domain UART relaying for multi-chip chains. When a
/// chain spans more than one domain boundary, the host programs the
/// first and last chip of each domain: `co_rel_en` enables relaying of
/// the command-out signal and `ro_rel_en` enables relaying of the
/// response-out signal. `gap_cnt` sets the inter-byte gap count the
/// relay inserts when forwarding the UART signal across the domain
/// boundary.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct UartRelayReg {
    /// Command-out relay enable. Set on the domain boundary chips to
    /// forward the command-out signal across the boundary.
    pub co_rel_en: bool,
    /// Response-out relay enable. Set on the domain boundary chips to
    /// forward the response-out signal across the boundary.
    pub ro_rel_en: bool,
    /// Opaque bits at positions 15:2; preserved but not characterized.
    #[bits(14)]
    pub unknown_bits_15_2: u16,
    /// Inter-byte gap count inserted by the relay when forwarding the
    /// UART signal across a chain domain boundary.
    pub gap_cnt: u16,
}

impl UartRelayReg {
    /// Builds a relay configuration with both command-out and
    /// response-out relay enabled and the given inter-byte gap count.
    /// Use this to program the boundary chips of each chain domain.
    #[must_use]
    pub const fn relays_enabled(gap_cnt: u16) -> Self {
        Self::new()
            .with_co_rel_en(true)
            .with_ro_rel_en(true)
            .with_gap_cnt(gap_cnt)
    }
}

impl Register for UartRelayReg {
    const REG_NUM: u8 = 0x2c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Per-core register addresses used by `CoreControlReg::reg_addr`.
/// Only the IDs the firmware programs are typed; values the chip
/// reports back outside that set decode as `Unknown(value)` so the
/// 5-bit payload survives a read-response round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CoreRegister {
    ClockDelayCtrl,
    CoreReg2,
    HashClockCtrl,
    CoreRegB,
    Unknown(u8),
}

impl CoreRegister {
    const fn into_bits(self) -> u8 {
        match self {
            Self::ClockDelayCtrl => 0x00,
            Self::CoreReg2 => 0x02,
            Self::HashClockCtrl => 0x05,
            Self::CoreRegB => 0x0b,
            Self::Unknown(v) => v & 0x1f,
        }
    }
    const fn from_bits(value: u8) -> Self {
        match value {
            0x00 => Self::ClockDelayCtrl,
            0x02 => Self::CoreReg2,
            0x05 => Self::HashClockCtrl,
            0x0b => Self::CoreRegB,
            v => Self::Unknown(v),
        }
    }
}

/// Per-core register access (`0x3c`).
///
/// Drives the chip's per-core register file: each write enqueues a
/// read or write of a [`CoreRegister`] inside one or more cores.
/// Read responses are returned on the next chip-to-host frame via
/// [`CoreRegisterValueReg`] at address `0x40`.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoreControlReg {
    /// Data byte. For writes, this lands in the selected core
    /// register; for reads, ignored.
    pub reg_wdata: u8,
    /// Target per-core register.
    #[bits(5)]
    pub reg_addr: CoreRegister,
    /// Reserved bit; preserved byte-exact.
    pub reserved_bit_13: bool,
    /// Opaque bit at position 14.
    pub unknown_bit_14: bool,
    /// Direction. `true` writes [`Self::reg_wdata`] into the selected
    /// register; `false` triggers a read.
    pub reg_wr: bool,
    /// Origin core ID for ranged or single-core operations.
    pub core_id: u8,
    /// Opaque bits at positions 30:24.
    #[bits(7)]
    pub unknown_bits_30_24: u8,
    /// High command bit (`DO_CMD`).
    pub do_cmd: bool,
}

impl CoreControlReg {
    /// Builds a broadcast write of `data` into the named per-core
    /// register across every core.
    #[must_use]
    pub const fn for_core_write(reg_addr: CoreRegister, data: u8) -> Self {
        Self::new()
            .with_do_cmd(true)
            .with_reg_wr(true)
            .with_reg_addr(reg_addr)
            .with_reg_wdata(data)
    }

    /// Default `HashClockCtrl` (0x05) write issued during post-PLL
    /// bring-up. Encodes byte-exact to
    /// [`CORE_CONTROL_DEFAULT_HASH_CLOCK`].
    #[must_use]
    pub const fn default_hash_clock_config() -> Self {
        Self::for_core_write(CoreRegister::HashClockCtrl, 0x40)
    }

    /// Default `ClockDelayCtrl` (0x00) write issued during post-PLL
    /// bring-up. Encodes byte-exact to
    /// [`CORE_CONTROL_DEFAULT_CLOCK_DELAY`].
    #[must_use]
    pub const fn default_clock_delay_config() -> Self {
        Self::for_core_write(CoreRegister::ClockDelayCtrl, 0x08)
    }

    /// Default `CoreReg2` (0x02) write issued during post-PLL
    /// bring-up. Encodes byte-exact to
    /// [`CORE_CONTROL_DEFAULT_CORE_REG2`].
    #[must_use]
    pub const fn default_core_reg2_config() -> Self {
        Self::for_core_write(CoreRegister::CoreReg2, 0xAA)
    }
}

/// `CoreControlReg` wire payload for the default `HashClockCtrl`
/// write — broadcast, write, value 0x40.
pub const CORE_CONTROL_DEFAULT_HASH_CLOCK: u32 = 0x8000_8540;

/// `CoreControlReg` wire payload for the default `ClockDelayCtrl`
/// write — broadcast, write, value 0x08.
pub const CORE_CONTROL_DEFAULT_CLOCK_DELAY: u32 = 0x8000_8008;

/// `CoreControlReg` wire payload for the default `CoreReg2`
/// write — broadcast, write, value 0xAA.
pub const CORE_CONTROL_DEFAULT_CORE_REG2: u32 = 0x8000_82AA;

impl Register for CoreControlReg {
    const REG_NUM: u8 = 0x3c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Per-core register response (`0x40`).
///
/// Read-back path for [`CoreControlReg`] read operations. Bits 15:8
/// are the `FOUND` byte; keep that byte raw rather than inferring a
/// returned core-register ID.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoreRegisterValueReg {
    /// Per-core register value reported by the chip.
    pub core_reg_value: u8,
    /// `FOUND` byte at bits 15:8.
    pub found: u8,
    /// Originating core ID.
    pub core_id: u8,
    #[bits(8)]
    __: u8,
}

impl Register for CoreRegisterValueReg {
    const REG_NUM: u8 = 0x40;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// External temperature-sensor read register (`0x44`).
///
/// Reports on-die and external temperature-diode ADC results. The
/// register is exposed as an opaque 32-bit value.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ExternalTemperatureSensorReg {
    /// Opaque ADC payload.
    pub value: u32,
}

impl Register for ExternalTemperatureSensorReg {
    const REG_NUM: u8 = 0x44;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Nonce-error counter register (`0x4c`).
///
/// Cumulative count of nonce errors detected by the chip. Read by
/// the host to monitor chip error rate.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NonceErrorCounterReg {
    /// Cumulative nonce-error count.
    pub count: u32,
}

impl Register for NonceErrorCounterReg {
    const REG_NUM: u8 = 0x4c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Analog mux control (`0x54`).
///
/// Selects which on-die diode or supply rail the analog mux exposes
/// for temperature / voltage measurement. The corresponding ADC
/// result is read from [`ExternalTemperatureSensorReg`] at address
/// `0x44`.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct AnalogMuxControl {
    /// Mux selector code; `0x03` shuts the mux off.
    pub ana_mux_sel: u32,
}

impl AnalogMuxControl {
    /// Mux-disabled setting written during pre-PLL bring-up. Shuts
    /// off the on-die temperature / voltage mux; encodes byte-exact
    /// to [`ANALOG_MUX_DISABLED`].
    #[must_use]
    pub const fn disabled() -> Self {
        Self::from_bits(ANALOG_MUX_DISABLED)
    }
}

impl Register for AnalogMuxControl {
    const REG_NUM: u8 = 0x54;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Mux-selector value that shuts off the on-die temperature /
/// voltage mux.
pub const ANALOG_MUX_DISABLED: u32 = 0x0000_0003;

/// I/O driver-strength control (`0x58`).
///
/// 4-bit drive-strength field per output pin (response, clock, reset,
/// busy, command), plus return-driver enables and RF drive strength.
/// [`IO_DRIVER_STRENGTH_DEFAULT`] captures the BM1362 default: every
/// low drive strength `0x1`, high fields zero.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct IoDriverStrengthCtrlReg {
    /// Command / TX output pin drive strength.
    #[bits(4)]
    pub co_ds: u8,
    /// Busy output pin drive strength.
    #[bits(4)]
    pub bo_ds: u8,
    /// Reset output pin drive strength.
    #[bits(4)]
    pub nrsto_ds: u8,
    /// Clock output (`RX_OUT`) pin drive strength.
    #[bits(4)]
    pub clko_ds: u8,
    /// Response / RX output pin drive strength.
    #[bits(4)]
    pub ro_ds: u8,
    /// Domain-0 return-driver enable.
    pub d0rs_en: bool,
    /// Domain-1 return-driver enable.
    pub d1rs_en: bool,
    /// Domain-2 return-driver enable.
    pub d2rs_en: bool,
    /// Domain-3 return-driver enable.
    pub d3rs_en: bool,
    /// RF output drive strength.
    #[bits(4)]
    pub rf_ds: u8,
    /// Reserved bits 31:28; BM1362 preset leaves zero.
    #[bits(4)]
    pub reserved_high: u8,
}

impl IoDriverStrengthCtrlReg {
    /// Default pre-PLL drive-strength preset: every drive strength
    /// set to `0x1`, reserved bits zero. Encodes byte-exact to
    /// [`IO_DRIVER_STRENGTH_DEFAULT`].
    #[must_use]
    pub const fn default_drive_strengths() -> Self {
        Self::new()
            .with_co_ds(1)
            .with_bo_ds(1)
            .with_nrsto_ds(1)
            .with_clko_ds(1)
            .with_ro_ds(1)
    }

    /// Drive-strength preset for a chain domain-boundary chip: identical
    /// to [`Self::default_drive_strengths`] except `clko_ds` is raised to
    /// `0xF` to boost the clock-out driver across the domain gap. Encodes
    /// byte-exact to [`IO_DRIVER_STRENGTH_DOMAIN_BOUNDARY`].
    #[must_use]
    pub const fn domain_boundary() -> Self {
        Self::new()
            .with_co_ds(1)
            .with_bo_ds(1)
            .with_nrsto_ds(1)
            .with_clko_ds(0xF)
            .with_ro_ds(1)
    }
}

impl Register for IoDriverStrengthCtrlReg {
    const REG_NUM: u8 = 0x58;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Default I/O driver-strength preset for the BM1362. Encodes
/// [`IoDriverStrengthCtrlReg::default_drive_strengths`].
pub const IO_DRIVER_STRENGTH_DEFAULT: u32 = 0x0001_1111;

/// Domain-boundary I/O driver-strength preset. Identical to
/// [`IO_DRIVER_STRENGTH_DEFAULT`] except `clko_ds` is `0xF` (clock-out
/// drive boost for chips that sit on a chain domain boundary). Encodes
/// [`IoDriverStrengthCtrlReg::domain_boundary`].
pub const IO_DRIVER_STRENGTH_DOMAIN_BOUNDARY: u32 = 0x0001_F111;

/// PLL0 divider register (`0x70`).
///
/// Programs the four 4-bit `W` divider bytes that feed PLL0. The
/// same layout repeats at addresses `0x74`, `0x78`, `0x7c` for
/// PLL1 / PLL2 / PLL3 dividers. The chip's effective divisor for
/// each lane is `byte + 1`; on BM1362 only the low byte is
/// programmed and the upper three bytes are written zero.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Pll0DividerReg {
    /// PLL0 lane-0 W divider, applied as `wdiv + 1`.
    pub wdiv: u8,
    /// Lane-1 W divider; BM1362 preset leaves zero.
    pub unknown_bits_15_8: u8,
    /// Lane-2 W divider; BM1362 preset leaves zero.
    pub unknown_bits_23_16: u8,
    /// Lane-3 W divider; BM1362 preset leaves zero.
    pub unknown_bits_31_24: u8,
}

impl Pll0DividerReg {
    /// All-zero payload: lane-0 divisor of 1, every other lane idle.
    /// The bring-up path pairs this with each PLL parameter write to
    /// fence the divider before and after the PLL update.
    #[must_use]
    pub const fn zeroed() -> Self {
        Self::from_bits(PLL0_DIVIDER_RESET)
    }
}

impl Register for Pll0DividerReg {
    const REG_NUM: u8 = 0x70;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// PLL0 divider value written before each PLL parameter update; all
/// bits zero (lane-0 divisor of 1, no extra divide).
pub const PLL0_DIVIDER_RESET: u32 = 0x0000_0000;

/// PLL1 divider register (`0x74`). Pairs with [`Pll1ParameterReg`] for
/// the BM1366 UART clock; the payload is the [`Pll0DividerReg`] layout
/// written to this instance address.
pub struct Pll1DividerReg(pub Pll0DividerReg);

impl Register for Pll1DividerReg {
    const REG_NUM: u8 = 0x74;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self(Pll0DividerReg::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// PLL3 divider register (`0x7c`). Pairs with [`Pll3ParameterReg`] for
/// the BM1370 UART clock; the payload is the [`Pll0DividerReg`] layout
/// written to this instance address.
pub struct Pll3DividerReg(pub Pll0DividerReg);

impl Register for Pll3DividerReg {
    const REG_NUM: u8 = 0x7c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self(Pll0DividerReg::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// Per-domain nonce / share counter for power domain 0 (`0x88`).
///
/// Cumulative count of returned nonces from cores in domain 0. Read
/// by the host to monitor per-domain throughput. The same layout
/// repeats at addresses `0x89`, `0x8a`, `0x8b` for domains 1, 2, 3.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Domain0NonceCounterReg {
    /// Cumulative nonce count for domain 0.
    pub count: u32,
}

impl Register for Domain0NonceCounterReg {
    const REG_NUM: u8 = 0x88;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Per-domain nonce / share counter for power domain 1 (`0x89`).
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Domain1NonceCounterReg {
    /// Cumulative nonce count for domain 1.
    pub count: u32,
}

impl Register for Domain1NonceCounterReg {
    const REG_NUM: u8 = 0x89;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Per-domain nonce / share counter for power domain 2 (`0x8a`).
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Domain2NonceCounterReg {
    /// Cumulative nonce count for domain 2.
    pub count: u32,
}

impl Register for Domain2NonceCounterReg {
    const REG_NUM: u8 = 0x8a;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Per-domain nonce / share counter for power domain 3 (`0x8b`).
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Domain3NonceCounterReg {
    /// Cumulative nonce count for domain 3.
    pub count: u32,
}

impl Register for Domain3NonceCounterReg {
    const REG_NUM: u8 = 0x8b;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// BM1366-family total nonce / share counter (`0x8c`).
///
/// Cumulative count of returned nonces across all four power domains.
/// This address carries a different meaning in other `BM13xx` family
/// layouts, so keep the semantics tied to the BM1366/BM1362 path.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TotalNonceCounterReg {
    /// Cumulative aggregate nonce count.
    pub count: u32,
}

impl Register for TotalNonceCounterReg {
    const REG_NUM: u8 = 0x8c;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Midstate calibration register (`0xa4`).
///
/// Configures version rolling: which version-field bits the chip may
/// toggle, plus the `EN` and `EN2` enable bits.
/// [`MIDSTATE_CAL_BRINGUP_BASELINE`] captures the production preset.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MidstateCalReg {
    /// Rolled version-field bitmask. Each set bit names a version-bit
    /// the chip may toggle while generating midstates.
    pub version_rolled_bitmask: u16,
    /// Reserved bits 27:16; BM1362 preset leaves zero.
    #[bits(12)]
    pub reserved_mid: u16,
    /// Second enable bit (`EN2`) at position 28.
    pub en2: bool,
    /// Opaque bit at position 29.
    pub unknown_bit_29: bool,
    /// Opaque bit at position 30.
    pub unknown_bit_30: bool,
    /// Primary enable bit (`EN`) at position 31.
    pub en: bool,
}

impl MidstateCalReg {
    /// Production midstate configuration: `EN` and `EN2` set, full
    /// 16-bit version-rolled bitmask.
    /// Encodes to [`MIDSTATE_CAL_BRINGUP_BASELINE`].
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::new()
            .with_en(true)
            .with_en2(true)
            .with_version_rolled_bitmask(0xFFFF)
    }
}

impl Register for MidstateCalReg {
    const REG_NUM: u8 = 0xa4;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// `MidstateCalReg` production value. Encodes
/// [`MidstateCalReg::factory_default`] byte-exact.
pub const MIDSTATE_CAL_BRINGUP_BASELINE: u32 = 0x9000_FFFF;

/// Soft-reset selector nibble (low 4 bits of
/// [`SoftResetControlReg`]). Each bit nominally selects one of the
/// four reset domains, plus the all / none combinations; the chip's
/// exact per-domain semantics are not verified here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DomainSelect {
    None = 0x0,
    Domain0 = 0x1,
    Domain1 = 0x2,
    Domain2 = 0x4,
    Domain3 = 0x8,
    All = 0xF,
}

impl DomainSelect {
    const fn into_bits(self) -> u8 {
        self as u8
    }
}

/// Soft-reset control (`0xa8`).
///
/// Carries the raw reset selector fields. The register is treated as
/// opaque raw nibbles plus raw bits `B8` and `B10`; no per-core or
/// per-domain reset semantics are asserted here.
/// [`SOFT_RESET_QUIESCENT`] is the idle value; [`SOFT_RESET_SRST_B1`]
/// sets bit 1 of the low selector nibble.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SoftResetControlReg {
    /// Raw low selector nibble `B3_0`. The chip does *not* clear this
    /// automatically.
    #[bits(4)]
    pub b3_0: u8,
    /// Raw selector nibble `B7_4` at bits 7:4.
    #[bits(4)]
    pub b7_4: u8,
    /// Raw bit B8. Semantics are family-specific.
    pub b8: bool,
    /// Unnamed raw bit 9.
    pub b9: bool,
    /// Raw bit B10.
    pub b10: bool,
    /// Reserved bits 15:11.
    #[bits(5)]
    pub reserved_mid: u8,
    /// Raw initialization byte at bits 23:16.
    pub b23_16: u8,
    /// Reserved byte 31:24; BM1362 preset leaves zero.
    pub reserved_high: u8,
}

impl SoftResetControlReg {
    /// Quiescent setting: every reset bit clear, raw byte 23:16 zero.
    /// Written during pre-enumeration so the chip is in a known
    /// state before address assignment. Encodes byte-exact to
    /// [`SOFT_RESET_QUIESCENT`].
    #[must_use]
    pub const fn quiescent() -> Self {
        Self::from_bits(SOFT_RESET_QUIESCENT)
    }

    /// Pulse the slow soft-reset on the requested domain; every other
    /// field zero. Per-chip post-PLL bring-up writes this to each
    /// chip in turn to clear residual core state before work dispatch
    /// begins. Encodes byte-exact to [`SOFT_RESET_SRST_B1`] when
    /// called with [`DomainSelect::Domain1`].
    #[must_use]
    pub const fn reset_core_domain(domain: DomainSelect) -> Self {
        Self::new().with_b3_0(domain.into_bits())
    }
}

impl Register for SoftResetControlReg {
    const REG_NUM: u8 = 0xa8;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// `SoftResetControlReg` quiescent value: every reset bit clear,
/// raw byte 23:16 zero.
pub const SOFT_RESET_QUIESCENT: u32 = 0x0000_0000;

/// `SoftResetControlReg` value with bit 1 of the low selector nibble
/// set (`b3_0 = 0x2`), every other field zero. Equivalent to
/// `SoftResetControlReg::new().with_b3_0(0x2)`.
pub const SOFT_RESET_SRST_B1: u32 = 0x0000_0002;

/// Misc settings register (`0xb9`).
///
/// Opaque 32-bit register whose field semantics are not characterized.
/// The BM1370 bring-up writes a fixed constant to it; the BM1362 does
/// not require it. Exposed as a raw value so a bring-up recipe can
/// write the known constant without claiming a field layout it cannot
/// verify.
#[bitfield(u32)]
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct MiscSettingsReg {
    /// Opaque misc-settings payload.
    pub value: u32,
}

impl MiscSettingsReg {
    /// Value the BM1370 bring-up writes to this register. Encodes
    /// byte-exact to [`MISC_SETTINGS_BM1370_BRINGUP`].
    #[must_use]
    pub const fn bm1370_bringup() -> Self {
        Self::from_bits(MISC_SETTINGS_BM1370_BRINGUP)
    }
}

impl Register for MiscSettingsReg {
    const REG_NUM: u8 = 0xb9;
    fn from_be_bytes(bytes: [u8; 4]) -> Self {
        Self::from_bits(u32::from_be_bytes(bytes))
    }
    fn to_be_bytes(&self) -> [u8; 4] {
        self.into_bits().to_be_bytes()
    }
}

/// Value the BM1370 bring-up writes to [`MiscSettingsReg`].
pub const MISC_SETTINGS_BM1370_BRINGUP: u32 = 0x0000_4480;

/// Crystal frequency driving the BM1362 PLL reference input. All
/// hash-clock targets derive from this constant via the PLL divider
/// chain.
pub const BASE_CLOCK: u64 = 25_000_000;

/// Inclusive operating-frequency envelope of a chip. The tuner ramps
/// inside this range and the register calculators reject targets outside
/// it.
#[derive(Clone, Copy)]
pub struct FrequencyRange {
    min: Frequency,
    max: Frequency,
}

impl FrequencyRange {
    /// Constructs the range from an inclusive lower and upper bound.
    #[must_use]
    pub const fn new(min: Frequency, max: Frequency) -> Self {
        Self { min, max }
    }

    /// Inclusive lower bound of the operating-frequency range.
    #[must_use]
    pub const fn min(self) -> Frequency {
        self.min
    }

    /// Inclusive upper bound of the operating-frequency range.
    #[must_use]
    pub const fn max(self) -> Frequency {
        self.max
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

/// Hashboard-level descriptor. Carries the chip model so the tuner
/// can dispatch the bring-up recipe and read chip capabilities through
/// a board-scoped seam; per-board topology fields land here alongside
/// the code that consumes them.
#[derive(Clone, Copy)]
pub struct BoardDescriptor {
    /// Chip model populated on this hashboard. Selects the bring-up
    /// recipe and capability bounds the tuner drives.
    pub chip: Chip,
    /// Number of voltage domains stacked in series on the rail. The
    /// PSU programs the sum across the stack, so the rail-side
    /// ceiling is `chip.max_chip_voltage * num_voltage_domains`.
    pub num_voltage_domains: u8,
    /// Number of chips on the serial chain. Drives the per-chip
    /// bring-up loops and the nonce-space division. Equals
    /// `num_voltage_domains` only when each chip is a single domain.
    pub num_chips: u8,
    /// Upper bound the PSU enforces on this board's rail. The
    /// `max_rail_voltage` accessor clamps the chip-stack product
    /// against this so `Psu::set_voltage` never returns
    /// `Error::OutOfRange` from a descriptor / PSU range mismatch.
    /// Matches the `end_mv()` of the `VoltageRange` the firmware
    /// configures on the board's PSU.
    // TODO: PSU-specific fields are currently bundled into the board
    // descriptor, which is acceptable for now, but should be split into
    // a dedicated PSU descriptor later.
    pub max_psu_voltage: Voltage,
}

impl BoardDescriptor {
    /// Open-core rail voltage: the per-chip maximum scaled by the
    /// number of series-stacked voltage domains, clamped against the
    /// PSU's configured upper bound. The tuner anchors the
    /// `V^2 * f` curve here before any clock is applied, and the
    /// rail walks down V as f climbs. Not `const`: it reads
    /// `Chip::descriptor`, a non-const trait method.
    #[must_use]
    pub fn max_rail_voltage(&self) -> Voltage {
        let chip_stack = self
            .chip
            .descriptor()
            .max_chip_voltage
            .mv()
            .saturating_mul(u32::from(self.num_voltage_domains));
        let psu_max = self.max_psu_voltage.mv();
        let clamped = if chip_stack < psu_max {
            chip_stack
        } else {
            psu_max
        };
        Voltage::from_mv(clamped)
    }
}

/// Hashboard descriptor. The `max_psu_voltage` mirrors
/// `MiniminerPsuConfig::DEFAULT.dcdc_voltage_range` (`VRange2`, end
/// 1675 mV).
pub const BMM100: BoardDescriptor = BoardDescriptor {
    chip: Chip::Bm1362,
    num_voltage_domains: 4,
    num_chips: 4,
    max_psu_voltage: Voltage::from_mv(1675),
};

impl PllParameterReg {
    /// REFDIV value the BM1362 PLL is programmed with; the PLL search
    /// collapses to a constant REFDIV.
    const PLL_REFDIV: u64 = 2;

    /// FBDIV register-value range for the BM1362 PLL. These bounds are
    /// local policy chosen to keep the divider chain in a usable
    /// window, not chip-recovered hard limits.
    const FBDIV_MIN: u8 = 160;
    const FBDIV_MAX: u8 = 237;

    /// POSTDIV1 stored-value range. The actual divisor applied by the
    /// chip is `stored + 1`.
    const POSTDIV1_STORED_MIN: u8 = 1;
    const POSTDIV1_STORED_MAX: u8 = 6;

    /// POSTDIV2 stored-value range. The actual divisor is `stored + 1`.
    /// The PLL search additionally constrains `POSTDIV2_stored <=
    /// POSTDIV1_stored`, enforced by the search loop bound rather than
    /// the constant itself.
    const POSTDIV2_STORED_MIN: u8 = 0;
    const POSTDIV2_STORED_MAX: u8 = 5;

    /// Hash clock in Hz produced by the divider tuple with WDIV pinned
    /// to 0:
    ///
    /// ```text
    /// freq_hz = BASE_CLOCK * FBDIV / REFDIV / (POSTDIV1+1) / (POSTDIV2+1)
    /// ```
    fn pll_freq_hz(fbdiv: u8, postdiv1_stored: u8, postdiv2_stored: u8) -> u64 {
        BASE_CLOCK * u64::from(fbdiv)
            / Self::PLL_REFDIV
            / (u64::from(postdiv1_stored) + 1)
            / (u64::from(postdiv2_stored) + 1)
    }

    /// Returns the PLL parameter register value that drives the chip
    /// closest to `target_khz`, or `None` if `target_khz` is outside
    /// the passed `range`.
    ///
    /// Iterates the `(POSTDIV2_stored, POSTDIV1_stored)` divider pairs
    /// and solves FBDIV directly from the PLL formula for each rather
    /// than scanning the FBDIV range: with REFDIV and both post-dividers
    /// fixed, `freq` is linear in FBDIV, so the nearest reachable clock
    /// for the pair is one of the two integer FBDIVs bracketing the
    /// ideal value. The best pair wins by sort key
    /// `(distance_to_target, POSTDIV2, FBDIV, POSTDIV1)`, so targets
    /// between grid points resolve to the nearest reachable hash clock.
    /// REFDIV is pinned to 2 and WDIV to 0. The divider bounds, the WDIV
    /// pin, and the tie-break key are local policy, not chip-recovered
    /// requirements. The chosen FBDIV also sets [`Self::vco_high`] via
    /// [`PLL_VCO_HIGH_THRESHOLD_HZ`].
    #[must_use]
    pub fn from_freq(target_khz: u32, range: FrequencyRange) -> Option<Self> {
        if !(range.min().khz()..=range.max().khz()).contains(&target_khz) {
            return None;
        }
        let goal_hz = u64::from(target_khz) * 1_000;
        let fbdiv_min = u64::from(Self::FBDIV_MIN);
        let fbdiv_max = u64::from(Self::FBDIV_MAX);

        let mut best: Option<(u64, u8, u8, u8)> = None;
        for postdiv2_stored in Self::POSTDIV2_STORED_MIN..=Self::POSTDIV2_STORED_MAX {
            let pd1_min = postdiv2_stored.max(Self::POSTDIV1_STORED_MIN);
            for postdiv1_stored in pd1_min..=Self::POSTDIV1_STORED_MAX {
                // freq = BASE * FBDIV / REFDIV / (pd1+1) / (pd2+1)
                // rearranges to FBDIV = freq * REFDIV * (pd1+1) * (pd2+1)
                // / BASE. Integer division floors the ideal value; the
                // floor and its successor bracket the nearest reachable
                // clock for this divider pair.
                let divisor_product =
                    (u64::from(postdiv1_stored) + 1) * (u64::from(postdiv2_stored) + 1);
                let ideal_fbdiv = goal_hz * Self::PLL_REFDIV * divisor_product / BASE_CLOCK;
                for bracket in [ideal_fbdiv, ideal_fbdiv + 1] {
                    // The clamp pins the candidate to the FBDIV range, so
                    // the cast to u8 cannot truncate.
                    #[allow(clippy::cast_possible_truncation)]
                    let fbdiv = bracket.clamp(fbdiv_min, fbdiv_max) as u8;
                    let freq = Self::pll_freq_hz(fbdiv, postdiv1_stored, postdiv2_stored);
                    let distance = freq.abs_diff(goal_hz);
                    let key = (distance, postdiv2_stored, fbdiv, postdiv1_stored);
                    if best.is_none_or(|current| key < current) {
                        best = Some(key);
                    }
                }
            }
        }

        best.map(|(_, postdiv2_stored, fbdiv, postdiv1_stored)| {
            let vco_hz = BASE_CLOCK * u64::from(fbdiv) / Self::PLL_REFDIV;
            Self::new()
                .with_pll_en(true)
                .with_vco_high(vco_hz >= PLL_VCO_HIGH_THRESHOLD_HZ)
                .with_fbdiv(fbdiv)
                .with_refdiv(PLL_REFDIV_VALUE)
                .with_postdiv1_minus_1(postdiv1_stored)
                .with_postdiv2_minus_1(postdiv2_stored)
        })
    }
}
