MEMORY {
    /* Zero-length anchor so the embassy-boot linker symbols below can
       compute offsets relative to the physical flash base (0x10000000)
       even though this binary links into the ACTIVE partition. */
    FLASH_ZERO       : ORIGIN = 0x10000000, LENGTH = 0

    /* Partition layout — must match the bootloader's memory.x exactly. */
    BOOTLOADER       : ORIGIN = 0x10000000, LENGTH = 128K
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    /* ACTIVE is where this application binary lives. ACTIVE and DFU split
       all the flash between the bootloader and config, so the A/B slots
       are as large as the layout allows. */
    FLASH            : ORIGIN = 0x10021000, LENGTH = 952K
    DFU              : ORIGIN = 0x1010F000, LENGTH = 956K

    RAM : ORIGIN = 0x20000000, LENGTH = 256K
}

/* Offsets from the physical flash base (FLASH_ZERO) for use by
   `FirmwareUpdaterConfig::from_linkerfile_blocking`.
   The subtraction of ORIGIN(FLASH_ZERO) converts absolute addresses to
   flash offsets, matching what embassy-boot expects. */

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH_ZERO);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH_ZERO);

__bootloader_active_start = ORIGIN(FLASH) - ORIGIN(FLASH_ZERO);
__bootloader_active_end   = ORIGIN(FLASH) + LENGTH(FLASH) - ORIGIN(FLASH_ZERO);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH_ZERO);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH_ZERO);
