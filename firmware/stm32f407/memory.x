MEMORY {
  FLASH  (rx) : ORIGIN = 0x08000000, LENGTH = 1024K
  /* RAM = SRAM1 (112K) + SRAM2 (16K) = 128K contiguous from 0x20000000 */
  RAM    (rw) : ORIGIN = 0x20000000, LENGTH = 128K
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

/* The main stack lives in CCM, growing down from its top toward
 * `.ccmram.bss`. In main RAM the stack had only the slack left over
 * after `.bss`/`.uninit` — under 200 bytes — with nothing but a
 * link-time assert between the executor's poll frames and the
 * statics below. CCM is never a DMA target (DMA buffers are statics
 * or task-future fields in main RAM), the core reaches it with zero
 * wait states, and hardware loads the initial SP from the vector
 * table before any code runs, so reset-time placement is safe.
 * Overriding `_stack_end` as well repoints cortex-m-rt's
 * stack-overlap assert at the real neighbour: a `.ccmram.bss` that
 * grows into the stack region now fails the link. */
_stack_start = ORIGIN(CCMRAM) + LENGTH(CCMRAM);
_stack_end = __eccmram_bss;

/* cortex-m-rt's assert above catches only an overlap, so it fires when
 * nothing is left at all. `.ccmram.bss` now carries the HTTP pool and
 * the metrics pool buffers as well, and each pool slot added there
 * takes 6 KiB of the stack's room, so hold a floor under what remains.
 * Embassy keeps task state in each task's own static future rather than
 * on the stack, so what this has to cover is the deepest single `poll`
 * plus interrupt frames. 16K is well above the deepest measured and
 * well below the ~29K the board links with today. */
_min_stack = 16K;
ASSERT(_stack_start - _stack_end >= _min_stack,
  "stack below the floor: .ccmram.bss has crowded CCM; move a static back to main RAM or shrink the HTTP pool or the metrics pool");
