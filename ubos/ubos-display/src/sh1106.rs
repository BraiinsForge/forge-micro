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

//! Minimal SH1106 OLED driver for 128x64 displays over SPI.

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

/// Display rotation. The SH1106 supports the identity orientation
/// and a 180° flip via segment-remap and COM-scan-dir bits in the
/// init sequence. 90° rotations would require software transposition
/// of the page-mapped framebuffer and are not supported.
#[derive(Clone, Copy)]
pub enum Rotation {
    /// Normal orientation.
    Normal,
    /// 180° rotation via segment-remap and COM-scan-dir flip.
    Rotate180,
}

/// Build the SH1106 init sequence with the rotation-dependent bytes
/// patched in. Two bytes change between orientations: segment remap
/// (0xA0/0xA1) and COM scan direction (0xC0/0xC8).
fn init_commands(rotation: Rotation) -> [u8; 23] {
    let (seg_remap, com_scan) = match rotation {
        Rotation::Normal => (0xA0, 0xC0),
        Rotation::Rotate180 => (0xA1, 0xC8),
    };
    [
        0xAE, // Display off.
        0xD5, 0x80, // Clock divide ratio / oscillator frequency.
        0xA8, 0x3F, // Multiplex ratio: 64 lines.
        0xD3, 0x00,      // Display offset: none.
        0x40,      // Display start line: 0.
        seg_remap, // Segment remap: 0xA0 normal, 0xA1 reversed (180°).
        com_scan,  // COM output scan direction: 0xC0 normal, 0xC8 reversed (180°).
        0xDA, 0x12, // COM pins hardware configuration: alternative.
        0x81, 0x7F, // Contrast: mid-range.
        0xA4, // Entire display ON follows RAM content.
        0xA6, // Normal display (not inverted).
        0xD9, 0xF1, // Pre-charge period.
        0xDB, 0x20, // VCOMH deselect level.
        0xAD, 0x8B, // DC-DC converter: enable.
        0xAF, // Display on.
    ]
}

/// SH1106 driver with an internal framebuffer.
///
/// Purpose-built because no maintained embedded-hal 1.0 driver exists.
/// Takes `SpiDevice` and DC `OutputPin` directly, bypassing the
/// `display-interface` abstraction layer.
pub struct Sh1106<SPI, DC> {
    spi: SPI,
    dc: DC,
    rotation: Rotation,
    buffer: [u8; BUFFER_SIZE],
}

impl<SPI, DC: OutputPin> Sh1106<SPI, DC> {
    /// Create a new driver. Call [`init`](Self::init) or
    /// [`init_async`](Self::init_async) before use.
    pub fn new(spi: SPI, dc: DC, rotation: Rotation) -> Self {
        Self {
            spi,
            dc,
            rotation,
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
        let commands = init_commands(self.rotation);
        self.send_commands(&commands)
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
        let commands = init_commands(self.rotation);
        self.send_commands_async(&commands).await
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
