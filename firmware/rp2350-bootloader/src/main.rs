//! RP2350 bootloader using `embassy-boot-rp`.
//!
//! Initialises the flash peripheral via `WatchdogFlash`, reads the partition
//! offsets baked into the linker symbols from `memory.x`, and chains to the
//! ACTIVE partition.  Control never returns from `load()`.

#![no_std]
#![no_main]

use core::cell::RefCell;

use cortex_m_rt::{entry, exception};
#[cfg(feature = "defmt")]
use defmt_rtt as _;
use embassy_boot_rp::{BootLoader, BootLoaderConfig, WatchdogFlash};
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

const FLASH_SIZE: usize = 2 * 1024 * 1024;

#[entry]
fn main() -> ! {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());

    let flash = WatchdogFlash::<FLASH_SIZE>::start(
        peripherals.FLASH,
        peripherals.WATCHDOG,
        Duration::from_secs(8),
    );
    let flash = Mutex::new(RefCell::new(flash));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bootloader: BootLoader = BootLoader::prepare(config);

    // Jump to the active partition. `FLASH_BASE` is the XIP base address
    // (0x10000000); adding the active partition offset gives the absolute
    // entry address the bootrom expects.
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
    // Read the lower 8 bits of ICSR (active exception number) and convert to
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
