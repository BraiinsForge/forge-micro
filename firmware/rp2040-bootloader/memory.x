MEMORY {
    /* The RP2040's bootrom loads the 256-byte second-stage boot2 from
       the first 256 bytes of flash, verifies its CRC32, then jumps into
       it. The bootloader code follows immediately after. Together they
       fill the 128 KiB BOOTLOADER region. */
    BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH            : ORIGIN = 0x10000100, LENGTH = 128K - 0x100
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    /* The binary that will be booted to. ACTIVE and DFU split all the
       flash between the bootloader and config, so the A/B slots are as
       large as the layout allows. */
    ACTIVE           : ORIGIN = 0x10021000, LENGTH = 952K
    /* New binary to swap in, or the previous binary to revert to if it failed.
       Must be at least one 4 KiB page larger than ACTIVE. */
    DFU              : ORIGIN = 0x1010F000, LENGTH = 956K
    RAM : ORIGIN = 0x20000000, LENGTH = 256K
}

/* embassy-rp embeds the 256-byte boot2 blob in `.boot2` via its
   `select_bootloader!` macro when `feature = "rp2040"` is active and
   `boot2-none` is not set. link-rp.x places that section at ORIGIN(BOOT2).
   EXTERN ensures the symbol is pulled in even if no other code references
   it directly. */
EXTERN(BOOT2_FIRMWARE)

/* These will be read by `BootLoaderConfig::from_linkerfile_blocking`.
   Mind the subtracted FLASH base: they are offsets in flash, not absolute addresses. */

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);
