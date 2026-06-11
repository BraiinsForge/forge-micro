//! Tunables for the BMM100 BM1362 mining backend.

/// Number of BM1362 chips on a BMM100 hashboard.
///
/// Matches `ubos_core::consts::Mining::NUM_CHIPS`; the value is
/// reproduced here so callers inside `ubos-mining` do not need to
/// reference the core crate for this single constant.
pub const NUM_CHIPS: u8 = ubos_core::consts::Mining::NUM_CHIPS;

/// Maximum chips the registry can address. Sized for the worst-case
/// chain length the protocol allows.
pub const MAX_CHIPS: usize = 16;

/// Hardware shares submitted at diff 1; the chip rolls below this and
/// hands every find back to the host.
pub const DIFFICULTY: u32 = 1;

/// Midstates per work frame. The chip iterates this many extranonce-rolled
/// variants of each header internally; the host picks `work_id` in
/// strides of `NUM_MIDSTATES` so the registry indexing stays aligned.
pub const NUM_MIDSTATES: usize = 8;

/// Slots in the in-memory registry that holds per-`work_id` context for
/// matching incoming `HwSolution` frames back to the originating job.
pub const REGISTRY_SIZE: usize = 256 / NUM_MIDSTATES;

/// Boot baud rate the chip listens on out of reset.
pub const INITIAL_BAUDRATE: u32 = 115_200;

/// Nominal fast baud requested after PLL programming. The chip's `0x28`
/// divider produces 3.125 Mbaud (`25 MHz / 8`) for this request; the
/// host UART is programmed to [`FAST_BAUDRATE_ACTUAL`] instead.
pub const FAST_BAUDRATE: u32 = 3_125_000;

/// Host-side UART rate the serial controller is programmed to when
/// [`FAST_BAUDRATE`] is requested. This is a host compatibility value,
/// not the chip's divider output (which is 3.125 Mbaud).
pub const FAST_BAUDRATE_ACTUAL: u32 = 3_000_000;
