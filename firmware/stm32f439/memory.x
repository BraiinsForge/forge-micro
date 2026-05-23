MEMORY {
  FLASH  (rx) : ORIGIN = 0x08000000, LENGTH = 2048K
  /* RAM = SRAM1 (112K) + SRAM2 (16K) + SRAM3 (64K) = 192K contiguous from 0x20000000 */
  RAM    (rw) : ORIGIN = 0x20000000, LENGTH = 192K
  /* CCMRAM = Core-Coupled Memory, 64K at 0x10000000; not DMA-accessible */
  CCMRAM (rw) : ORIGIN = 0x10000000, LENGTH = 64K
}

SECTIONS {
  .ccmram.bss (NOLOAD) : ALIGN(4) {
    __sccmram_bss = .;
    *(.ccmram.bss .ccmram.bss.*)
    . = ALIGN(4);
    __eccmram_bss = .;
  } > CCMRAM
} INSERT AFTER .bss;
