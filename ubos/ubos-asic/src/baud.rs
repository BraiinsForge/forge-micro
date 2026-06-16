//! Compile-time baud-rate derivation for the BM1370 fast-UART path.
//!
//! The BM1370 on the hashnode board has marginal signal integrity on its
//! chip serial pair. Running the chip at the generic 3.125 Mbaud (bt8d=0)
//! causes framing errors and resyncs; the validated safe rate is 520 833 Hz
//! (bt8d=5), which increases the per-bit sampling window by roughly 6x. Both the
//! chip register value and the host line rate are derived from the same
//! bt8d so they cannot drift apart.
//!
//! `MINING_BAUDRATE` (set in the hashnode `.cargo/config.toml`) overrides
//! the default at build time. Non-digit input fails the build rather than
//! silently mis-clocking the chips.

/// UART reference clock on the BM1370, in Hz.
const UART_CLK_HZ: u32 = 25_000_000;

/// Compile-time parse of a decimal baud-rate env string, with a fallback.
///
/// Non-digit input fails the build rather than silently mis-clocking the
/// chips; the caller's `520_833` default corresponds to bt8d=5.
const fn baud_env_to_hz(env: Option<&str>, default_hz: u32) -> u32 {
    let Some(text) = env else {
        return default_hz;
    };
    let bytes = text.as_bytes();
    let mut baud: u32 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let digit = bytes[index];
        assert!(
            digit.is_ascii_digit(),
            "MINING_BAUDRATE must be decimal digits"
        );
        baud = baud * 10 + (digit - b'0') as u32;
        index += 1;
    }
    assert!(baud > 0, "MINING_BAUDRATE must be a positive integer");
    baud
}

/// Requested baud rate for the BM1370 fast-UART phase, in Hz.
///
/// Defaults to `520_833` (bt8d=5), the rate validated on the hashnode: its
/// chip serial pair has marginal signal integrity, so the 3.125 Mbaud the chip
/// would otherwise run accumulates framing errors and resyncs; the slower
/// rate increases the per-bit sampling window by roughly 6x. Both the chip register
/// and the host divider derive from this one value so they cannot drift apart.
const MINING_BAUDRATE_HZ: u32 = baud_env_to_hz(option_env!("MINING_BAUDRATE"), 520_833);

/// bt8d divider for [`MINING_BAUDRATE_HZ`], derived at build time.
///
/// `bt8d = round(uart_clk / (8 * baud)) - 1`. The chip register encoding
/// uses `uart_clk / ((bt8d + 1) * 8)`, so deriving bt8d and then the exact
/// host rate from it ensures chip and host stay matched even when the env
/// baud does not divide evenly.
/// bt8d divider for the BM1370 fast-UART rate. `BaudRateReg::bm1370_fast`
/// loads it into the register's `bt8d` field on top of the shared fast-UART
/// preset, so the chip register and the host rate stay derived from this one
/// value. For the default 520 833 Hz build this is 5.
pub const BM1370_FAST_UART_BT8D: u8 = {
    // Integer rounding: (numerator + half_denom) / denom.
    let half = (8 * MINING_BAUDRATE_HZ) / 2;
    let bt8d = (UART_CLK_HZ + half) / (8 * MINING_BAUDRATE_HZ) - 1;
    assert!(
        bt8d <= 0xFF,
        "MINING_BAUDRATE produces a bt8d value that does not fit in one byte"
    );
    // The assert above proves the value fits in a u8.
    #[allow(clippy::cast_possible_truncation)]
    {
        bt8d as u8
    }
};

/// Host line rate for the BM1370 fast-UART phase, in Hz.
///
/// Derived from [`BM1370_FAST_UART_BT8D`] (`uart_clk / ((bt8d + 1) * 8)`)
/// so the host divider cannot drift from the chip's actual line rate.
/// For the default build this is `520_833`.
pub const BM1370_FAST_UART_HOST_HZ: u32 = UART_CLK_HZ / ((BM1370_FAST_UART_BT8D as u32 + 1) * 8);
