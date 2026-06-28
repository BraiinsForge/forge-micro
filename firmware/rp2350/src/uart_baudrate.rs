//! `serial_baudrate::BaudRate` wrapper for embassy-rp `BufferedUart` halves.
//!
//! embassy-rp 0.10 exposes runtime baud rate switching only on the unsplit
//! `BufferedUart` via `&mut self`; the per-half `info: &'static Info` field
//! that drives `set_baudrate_inner` is private. `ubos_mining::serial::Serial::run`
//! takes two independent halves, each implementing
//! `serial_baudrate::BaudRate(&self, ...)`. This module wraps each half with
//! the UART register block and re-implements the divisor math against the
//! PAC, mirroring `embassy_rp::uart::Uart::set_baudrate_inner`.

use embassy_rp::pac::uart::Uart as UartRegs;
use embassy_rp::pac::uart::regs::{Uartfbrd, Uartibrd};
use embassy_rp::uart::{BufferedUart, Error as UartError};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// PL011 ibrd field is 16 bits; the divisor math saturates at this value
/// just like the reference implementation in embassy-rp.
const MAX_INTEGER_DIVISOR: u32 = 65_535;

/// The BM1362 serial link: a `BufferedUart` plus the underlying register block
/// so `BaudRate::set_baudrate` can take `&self`. `BufferedUart` is full-duplex
/// and already implements `Read` + `Write`, so the ubos-mining actor drives it
/// as one instance rather than split halves.
pub struct MinerUart {
    inner: BufferedUart,
    regs: UartRegs,
}

impl MinerUart {
    /// Wrap a `BufferedUart` built from `UART1`, hard-coding
    /// `embassy_rp::pac::UART1` as the register block so the baud divisor is
    /// always written to the right peripheral. The only `BufferedUart` in this
    /// firmware lives on `UART1`.
    #[must_use]
    pub fn new(uart: BufferedUart) -> Self {
        Self {
            inner: uart,
            regs: embassy_rp::pac::UART1,
        }
    }
}

impl ErrorType for MinerUart {
    type Error = UartError;
}

impl Read for MinerUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buffer).await
    }
}

impl Write for MinerUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

impl BaudRate for MinerUart {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_baudrate(self.regs, baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_baudrate(self.regs)
    }
}

/// Replicates `embassy_rp::uart::Uart::set_baudrate_inner` plus the
/// `lcr_modify` enable/disable dance the PL011 needs around an `LCR_H` write.
/// The ubos-mining actor wraps every call with a one-second drain on either side, so
/// the wait-for-in-flight-character logic in embassy-rp's version is dropped.
fn write_baudrate(regs: UartRegs, baud: u32) -> Result<(), ErrorKind> {
    if baud == 0 {
        return Err(ErrorKind::InvalidInput);
    }
    let clk_base = embassy_rp::clocks::clk_peri_freq();
    let baud_rate_div = 8 * clk_base / baud;
    let mut integer_divisor = baud_rate_div >> 7;
    let mut fractional_divisor = (baud_rate_div & 0x7f).div_ceil(2);
    if integer_divisor == 0 {
        integer_divisor = 1;
        fractional_divisor = 0;
    } else if integer_divisor >= MAX_INTEGER_DIVISOR {
        integer_divisor = MAX_INTEGER_DIVISOR;
        fractional_divisor = 0;
    }

    // The PL011 latches IBRD/FBRD on the next LCR_H write, and writing LCR_H
    // while the UART is enabled is undefined. Disable UARTEN/TXE/RXE,
    // commit, then restore CR. The ubos-mining 1 s drain around this call covers any
    // bytes already on the wire.
    let cr = regs.uartcr().read();
    if cr.uarten() {
        regs.uartcr().modify(|w| {
            w.set_uarten(false);
            w.set_txe(false);
            w.set_rxe(false);
        });
    }
    regs.uartibrd().write_value(Uartibrd(integer_divisor));
    regs.uartfbrd().write_value(Uartfbrd(fractional_divisor));
    let lcr = regs.uartlcr_h().read();
    regs.uartlcr_h().write_value(lcr);
    if cr.uarten() {
        regs.uartcr().write_value(cr);
    }
    Ok(())
}

fn read_baudrate(regs: UartRegs) -> Result<u32, ErrorKind> {
    let integer_divisor = u32::from(regs.uartibrd().read().baud_divint());
    let fractional_divisor = u32::from(regs.uartfbrd().read().baud_divfrac());
    let baud_rate_div = (integer_divisor << 7) | (fractional_divisor * 2);
    if baud_rate_div == 0 {
        return Err(ErrorKind::InvalidData);
    }
    let clk_base = embassy_rp::clocks::clk_peri_freq();
    Ok(8 * clk_base / baud_rate_div)
}
