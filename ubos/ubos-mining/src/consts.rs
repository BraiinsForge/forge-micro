//! Tunables for the BM1362 and BM1370 mining backends.

/// Maximum chips the registry can address and the actor buffers replies
/// for. Tied to the chip-protocol chain cap so a discovery read can
/// collect every responder the chain may hold.
pub const MAX_CHIPS: usize = ubos_asic::MAX_CHAIN_CHIPS;

/// Hardware shares submitted at diff 1; the chip rolls below this and
/// hands every find back to the host.
pub const DIFFICULTY: u32 = 1;

/// Slots in the in-memory registry that holds per-`work_id` context for
/// matching incoming `HwSolution` frames back to the originating job.
/// Sized for the smallest supported per-job work-id count (8), i.e.
/// `256 / 8`; chips with a larger count use fewer of these slots.
pub const WORK_REGISTRY_SLOTS: usize = 256 / 8;

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
