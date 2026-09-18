MEMORY {
    /* Bootloader code lives at the start of flash. */
    FLASH            : ORIGIN = 0x10000000, LENGTH = 128K
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    /* The binary that will be booted to. ACTIVE and DFU split all the
       flash between the bootloader and config, so the A/B slots are as
       large as the layout allows. */
    ACTIVE           : ORIGIN = 0x10021000, LENGTH = 952K
    /* New binary to swap in, or the previous binary to revert to if it failed.
       Must be at least one 4 KiB page larger than ACTIVE. */
    DFU              : ORIGIN = 0x1010F000, LENGTH = 956K
    RAM : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes after .vector_table, to keep it in the first 4K of flash
     * where the Boot ROM (and picotool) can find it.
     */
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
    /* ### Boot ROM extra info
     *
     * Goes after everything in our program, so it can contain a signature.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;


PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

/* These will be read by `BootLoaderConfig::from_linkerfile_blocking`.
   Mind the subtracted FLASH base: they are offsets in flash, not absolute addresses. */

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);
