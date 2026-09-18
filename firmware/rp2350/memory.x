MEMORY {
    /* Zero-length anchor so the embassy-boot linker symbols below can
       compute offsets relative to the physical flash base (0x10000000)
       even though this binary links into the ACTIVE partition. */
    FLASH_ZERO       : ORIGIN = 0x10000000, LENGTH = 0

    /* Partition layout — must match the bootloader's memory.x exactly. */
    BOOTLOADER       : ORIGIN = 0x10000000, LENGTH = 128K
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    /* ACTIVE is where this application binary lives. ACTIVE and DFU split
       the flash between the bootloader and config; DFU is one sector larger
       as embassy-boot's swap scratch space. */
    FLASH            : ORIGIN = 0x10021000, LENGTH = 952K
    DFU              : ORIGIN = 0x1010F000, LENGTH = 956K

    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
    SRAM8 : ORIGIN = 0x20080000, LENGTH = 4K
    SRAM9 : ORIGIN = 0x20081000, LENGTH = 4K
}

SECTIONS {
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH

} INSERT AFTER .vector_table;

/* Rounded up so .text starts 8-aligned: the block before it is 4-aligned
   and its size is not a multiple of 8, and the linker warns when a section
   asks for more alignment than its address has. */
_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), 8);

SECTIONS {
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

/* Offsets from the physical flash base (FLASH_ZERO) for use by
   `FirmwareUpdaterConfig::from_linkerfile_blocking` in Stage 2.
   The subtraction of ORIGIN(FLASH_ZERO) converts absolute addresses to
   flash offsets, matching what embassy-boot expects. */

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH_ZERO);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH_ZERO);

__bootloader_active_start = ORIGIN(FLASH) - ORIGIN(FLASH_ZERO);
__bootloader_active_end   = ORIGIN(FLASH) + LENGTH(FLASH) - ORIGIN(FLASH_ZERO);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH_ZERO);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH_ZERO);
