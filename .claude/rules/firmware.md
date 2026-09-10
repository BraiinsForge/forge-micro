---
paths:
  - "firmware/**/*"
  - "ubos/**/*"
---

# Firmware Conventions

Implementation conventions for the firmware crates and the shared `ubos/`
libraries. These load only when working under `firmware/` or `ubos/`.

- All crates use `portable-atomic` for a uniform atomics API across targets.
- The cortex-m firmware (rp2040, rp2350, stm32f407, stm32f439) uses
  `#[cortex_m_rt::entry]` and drives `embassy_executor::raw::Executor`
  via `ubos_metrics::cpu_load::run_cortex_m_executor` so the poll/wfe
  loop can bracket `wfe` with `Instant::now()` reads. bmm-pico stays
  on `#[embassy_executor::main]` because its core-1 split runs a
  separate hand-managed executor for the `ubos-mining` serial worker;
  the busy-ratio metric is intentionally absent there.
- The esp32s3 firmware uses `#[esp_hal::main]` and constructs the
  esp-rtos embassy executor manually so it can pass idle hooks to
  `Executor::run_with_callbacks`.
- All boards share network tasks, constants, buffers, and response
  building via the `ubos-core`, `ubos-net`, and `ubos-metrics` crates
  under `ubos/`.
- Embassy task constructors return `Result<SpawnToken, SpawnError>`;
  `spawner.spawn()` returns `()`. The `unwrap!` wraps the constructor.
- ESP32 crate tracks `esp-rtos 0.3.0` / `esp-hal 1.1` and uses the
  workspace `embassy-executor 0.10`. esp-rtos 0.3.0 added stack-overflow
  detection (it panics `stack pointer out of range` instead of silently
  corrupting) and fixed the multi-core task-switch interrupt nesting that
  the 0.2.0 line corrupted under load.
- UART for the BM1362 chip serial link uses DMA on cortex-m targets:
  `embassy-stm32` `RingBufferedUartRx` for stm32f407/stm32f439, and
  `embassy-rp` `BufferedUart` (interrupt-driven, 1 KiB software ring
  buffer each direction) for rp2040/rp2350/bmm-pico. The RP hardware
  FIFO is 32 bytes; one-shot DMA would overflow it between `read()`
  calls at 921600 baud, and embassy-rp 0.10.0 ships no ring-buffered
  DMA RX. `BufferedUart` already implements `embedded_io_async::Read`
  and `Write` directly, so no newtype adapter is needed. The esp32s3
  board firmware stays on the interrupt+FIFO `Uart::into_async()` path.
  esp-hal 1.1.1 now ships the UHCI UART-over-DMA driver (`esp_hal::uart::uhci`,
  behind the `unstable` feature, which this crate enables), closing the
  esp-rs/esp-hal#2642 / #4720 gap that 1.0.0 had. UHCI RX supports idle-EOF
  framing, so it suits the request/response chip protocol, but switching is
  deferred: the 128-byte FIFO already wakes on a single RX byte, DMA only buys
  high-baud jitter headroom we have no evidence of needing, and the RX path
  must not be rewritten while it cannot be verified on hardware. All dual-core
  boards (rp2040, rp2350, bmm-pico, esp32s3) pin the
  ubos-mining `serial_task` to Core 1 so embassy-net on Core 0 stays
  responsive; the shared `ASIC_SESSION` uses `CriticalSectionRawMutex`,
  so cross-core sharing is free. On esp32s3, Core 1's executor also runs
  `IdleAccounting::<1>` so core 1's idle ticks are recorded for cpu-load
  metrics. The S3's 128-byte hardware FIFO absorbs the ETH-ISR-class
  jitter that overran the 1-byte USART holding register on stm32f407.

## Free functions

A function at module level, with no receiver, is correct in six cases
and a method otherwise. The Rust API Guidelines decide the line:
C-METHOD asks for a clear receiver, and names what the method form
gains — no import at the call site, an automatic borrow, and one place
in `rustdoc` where a reader finds the operation. A conversion that gains
none of the three gains nothing, so `crc8(bytes)` stays free and a
function that threads one parameter through eight call sites by hand
becomes a method on it.

1. **Pure transform.** Primitives or wire bytes in, primitives or wire
   bytes out, and no state. `str::from_utf8` is the precedent.
2. **One instance by construction.** A facade over a private `static`,
   or a hardware singleton reached through a peripheral token, such as
   a board's `init`.
3. **The receiver cannot be a local type.** An inherent `impl` needs a
   type the crate declares. A foreign type or a generic parameter cannot
   take one, and the remedies — an extension trait or a newtype — each
   add an item. The free function stays correct until the module accepts
   one of them.
4. **Fixed by a framework.** Entry points, interrupt handlers, tasks, and
   everything else whose shape a macro or the linker fixes.
5. **A split borrow under one lock.** The caller takes the lock once and
   hands each helper a disjoint field of the locked state. A method on
   the handle takes the lock again and deadlocks.
6. **Two peer parameters.** The operation drives two values of equal
   standing, such as a chain driver and its transport, and neither one
   is the receiver.

A class-1 function needs no note: the signature carries the argument.
Classes 2, 3, 5 and 6 are invisible from the signature, so the reason is
written down. A reason that covers several functions in one module goes
in the module doc, once. A reason that belongs to one signature goes on
that function. A constructor is an associated function on its type,
`Type::new`, never a free `new_type()`.
