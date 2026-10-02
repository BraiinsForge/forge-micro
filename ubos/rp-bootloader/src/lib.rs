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

//! Shared `embassy-boot-rp` swap sequence for the RP2040 and RP2350
//! bootloaders.
//!
//! The partition offsets come from linker symbols, so the calling crate
//! keeps its own `memory.x`, picks the flash size and chip feature, and
//! wraps `boot` in a `#[entry]` `main`.

#![no_std]

use core::cell::RefCell;

use cortex_m_rt::exception;
#[cfg(feature = "defmt")]
use defmt_rtt as _;
use embassy_boot_rp::{BootLoader, BootLoaderConfig, WatchdogFlash};
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

/// `WatchdogFlash` feeds the watchdog after every block, so this bounds a
/// stuck swap rather than ordinary boot time.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(8);

/// Runs the swap-bootloader sequence and jumps to the ACTIVE partition.
///
/// `FLASH_SIZE` is the chip's total flash size in bytes.
pub fn boot<const FLASH_SIZE: usize>() -> ! {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());

    let flash = WatchdogFlash::<FLASH_SIZE>::start(
        peripherals.FLASH,
        peripherals.WATCHDOG,
        WATCHDOG_TIMEOUT,
    );
    let flash = Mutex::new(RefCell::new(flash));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bootloader: BootLoader = BootLoader::prepare(config);

    // `FLASH_BASE` is the XIP base address, so adding the active partition
    // offset to it gives the absolute entry address the bootrom expects.
    unsafe { bootloader.load(embassy_rp::flash::FLASH_BASE as u32 + active_offset) }
}

#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".HardFault.user"))]
unsafe extern "C" fn HardFault() {
    cortex_m::peripheral::SCB::sys_reset();
}

#[exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    const SCB_ICSR: *const u32 = 0xE000_ED04 as *const u32;
    // Read the active exception number, the lower 8 bits of ICSR, and convert to
    // the signed irqn convention: external IRQ 0 is exception 16.
    let exception_number =
        u8::try_from(unsafe { core::ptr::read_volatile(SCB_ICSR) } & 0xFF).unwrap_or(0);
    let irqn = i16::from(exception_number) - 16;

    panic!("DefaultHandler #{irqn:?}");
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    cortex_m::asm::udf();
}
