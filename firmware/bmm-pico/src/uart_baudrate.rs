//! `serial_baudrate::BaudRate` wrapper for embassy-rp `BufferedUart` halves.
//! Re-implements the PL011 divisor math against the PAC so each half can
//! retune the line with a `&self` receiver.
//!
//! Backend architecture: `../../../docs/MMM.md`.

use embassy_rp::pac::uart::regs::{Uartfbrd, Uartibrd};
use embassy_rp::pac::uart::Uart as UartRegs;
use embassy_rp::uart::{BufferedUart, BufferedUartRx, BufferedUartTx, Error as UartError};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use serial_baudrate::BaudRate;

/// PL011 ibrd field is 16 bits; the divisor math saturates at this value
/// just like the reference implementation in embassy-rp.
const MAX_INTEGER_DIVISOR: u32 = 65_535;

/// Receiving half of a `BufferedUart` plus the underlying register block so
/// `BaudRate::set_baudrate` can take `&self`.
pub struct MmmUartRx {
    inner: BufferedUartRx,
    regs: UartRegs,
}

/// Transmitting half of a `BufferedUart` plus the underlying register block
/// so `BaudRate::set_baudrate` can take `&self`.
pub struct MmmUartTx {
    inner: BufferedUartTx,
    regs: UartRegs,
}

/// Split a `BufferedUart` built from `UART1` into the two wrapper halves,
/// hard-coding `embassy_rp::pac::UART1` as the register block so either side
/// can retune the line. Hard-coding closes the foot-gun that comes with
/// taking the PAC handle as a separate argument: a `pac::UART0`/`UART1` mix-up
/// would compile but write baud divisors to the wrong peripheral. The only
/// `BufferedUart` in this firmware lives on `UART1`; if a future board adds a
/// second instance, mint a `split_uart0` alongside this one.
#[must_use]
pub fn split_uart1(uart: BufferedUart) -> (MmmUartTx, MmmUartRx) {
    let (tx, rx) = uart.split();
    let regs = embassy_rp::pac::UART1;
    (MmmUartTx { inner: tx, regs }, MmmUartRx { inner: rx, regs })
}

impl ErrorType for MmmUartRx {
    type Error = UartError;
}

impl Read for MmmUartRx {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buffer).await
    }
}

impl ErrorType for MmmUartTx {
    type Error = UartError;
}

impl Write for MmmUartTx {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await
    }
}

impl BaudRate for MmmUartTx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_baudrate(self.regs, baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_baudrate(self.regs)
    }
}

impl BaudRate for MmmUartRx {
    fn set_baudrate(&self, baud: u32) -> Result<(), ErrorKind> {
        write_baudrate(self.regs, baud)
    }

    fn baudrate(&self) -> Result<u32, ErrorKind> {
        read_baudrate(self.regs)
    }
}

/// Replicates `embassy_rp::uart::Uart::set_baudrate_inner` plus the
/// `lcr_modify` enable/disable dance the PL011 needs around an `LCR_H` write.
/// The mmm actor wraps every call with a one-second drain on either side, so
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
    // commit, then restore CR. The mmm 1 s drain around this call covers any
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
