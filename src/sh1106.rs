//! Minimal SH1106 OLED driver for 128x64 displays over SPI.
//!
//! The SH1106 has no maintained embedded-hal 1.0 driver. This is a
//! purpose-built replacement: blocking and async page-writes from an
//! in-memory framebuffer, just enough for `DrawTarget<Color =
//! BinaryColor>`.
//!
//! Takes an `SpiDevice` and a DC (data/command) `OutputPin` directly,
//! avoiding the `display-interface` abstraction.

use core::convert::Infallible;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_hal::digital::OutputPin;

const WIDTH: u32 = 128;
const HEIGHT: u32 = 64;
const WIDTH_USIZE: usize = WIDTH as usize;

/// Number of 8-pixel-high pages covering the display (64 / 8 = 8).
const PAGES: u8 = 8;

/// SH1106 has 132 columns of RAM but only 128 visible. The visible
/// area starts at column 2.
const COLUMN_OFFSET: u8 = 2;

/// Framebuffer size: one byte per column per page.
const BUFFER_SIZE: usize = WIDTH_USIZE * PAGES as usize;

/// SH1106 initialization command sequence.
const INIT_COMMANDS: &[u8] = &[
    0xAE, // Display off.
    0xD5, 0x80, // Clock divide ratio / oscillator frequency.
    0xA8, 0x3F, // Multiplex ratio: 64 lines.
    0xD3, 0x00, // Display offset: none.
    0x40, // Display start line: 0.
    0xA0, // Segment remap: column 0 mapped to SEG0.
    0xC0, // COM output scan direction: normal.
    0xDA, 0x12, // COM pins hardware configuration: alternative.
    0x81, 0x7F, // Contrast: mid-range.
    0xA4, // Entire display ON follows RAM content.
    0xA6, // Normal display (not inverted).
    0xD9, 0xF1, // Pre-charge period.
    0xDB, 0x20, // VCOMH deselect level.
    0xAD, 0x8B, // DC-DC converter: enable.
    0xAF, // Display on.
];

/// SH1106 driver with an internal framebuffer.
pub struct Sh1106<SPI, DC> {
    spi: SPI,
    dc: DC,
    buffer: [u8; BUFFER_SIZE],
}

impl<SPI, DC: OutputPin> Sh1106<SPI, DC> {
    /// Create a new driver. Call [`init`](Self::init) or
    /// [`init_async`](Self::init_async) before use.
    pub fn new(spi: SPI, dc: DC) -> Self {
        Self {
            spi,
            dc,
            buffer: [0; BUFFER_SIZE],
        }
    }

    /// Send command bytes: DC low, then SPI write.
    fn send_commands(&mut self, commands: &[u8]) -> Result<(), SPI::Error>
    where
        SPI: embedded_hal::spi::SpiDevice,
    {
        // DC pin is Infallible on all target platforms.
        let _ = self.dc.set_low();
        self.spi.write(commands)
    }

    /// Send command bytes asynchronously: DC low, then SPI write.
    async fn send_commands_async(&mut self, commands: &[u8]) -> Result<(), SPI::Error>
    where
        SPI: embedded_hal_async::spi::SpiDevice,
    {
        let _ = self.dc.set_low();
        self.spi.write(commands).await
    }
}

// These methods use blocking SPI.
impl<SPI: embedded_hal::spi::SpiDevice, DC: OutputPin> Sh1106<SPI, DC> {
    /// Send the SH1106 initialization sequence.
    ///
    /// # Errors
    ///
    /// Returns the SPI error if any command transfer fails.
    pub fn init(&mut self) -> Result<(), SPI::Error> {
        self.send_commands(INIT_COMMANDS)
    }

    /// Write the framebuffer to the display, one page at a time.
    ///
    /// # Errors
    ///
    /// Returns the SPI error if any transfer fails.
    pub fn flush(&mut self) -> Result<(), SPI::Error> {
        for page in 0..PAGES {
            let commands: [u8; 3] = [
                0xB0 | page,
                COLUMN_OFFSET & 0x0F,
                0x10 | (COLUMN_OFFSET >> 4),
            ];
            let _ = self.dc.set_low();
            self.spi.write(&commands)?;
            let start = usize::from(page) * WIDTH_USIZE;
            let _ = self.dc.set_high();
            self.spi.write(&self.buffer[start..start + WIDTH_USIZE])?;
        }
        Ok(())
    }
}

// These methods use async SPI.
impl<SPI: embedded_hal_async::spi::SpiDevice, DC: OutputPin> Sh1106<SPI, DC> {
    /// Send the SH1106 initialization sequence asynchronously.
    ///
    /// # Errors
    ///
    /// Returns the SPI error if any command transfer fails.
    pub async fn init_async(&mut self) -> Result<(), SPI::Error> {
        self.send_commands_async(INIT_COMMANDS).await
    }

    /// Write the framebuffer to the display asynchronously, one page
    /// at a time.
    ///
    /// # Errors
    ///
    /// Returns the SPI error if any transfer fails.
    pub async fn flush_async(&mut self) -> Result<(), SPI::Error> {
        for page in 0..PAGES {
            let commands: [u8; 3] = [
                0xB0 | page,
                COLUMN_OFFSET & 0x0F,
                0x10 | (COLUMN_OFFSET >> 4),
            ];
            let _ = self.dc.set_low();
            self.spi.write(&commands).await?;
            let start = usize::from(page) * WIDTH_USIZE;
            let _ = self.dc.set_high();
            self.spi
                .write(&self.buffer[start..start + WIDTH_USIZE])
                .await?;
        }
        Ok(())
    }
}

impl<SPI, DC> DrawTarget for Sh1106<SPI, DC> {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let Ok(x) = usize::try_from(point.x) else {
                continue;
            };
            let Ok(y) = usize::try_from(point.y) else {
                continue;
            };
            if x >= WIDTH_USIZE || y >= HEIGHT as usize {
                continue;
            }
            let page = y / 8;
            let bit = y % 8;
            let index = page * WIDTH_USIZE + x;
            match color {
                BinaryColor::On => self.buffer[index] |= 1 << bit,
                BinaryColor::Off => self.buffer[index] &= !(1 << bit),
            }
        }
        Ok(())
    }
}

impl<SPI, DC> OriginDimensions for Sh1106<SPI, DC> {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}
