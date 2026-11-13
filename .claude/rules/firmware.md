---
paths:
  - "firmware/**/*"
  - "ubos/**/*"
---

# Firmware Conventions

Conventions for the firmware crates and the shared `ubos/` libraries.

- All crates use `portable-atomic`, so atomics read the same on every
  target.
- The cortex-m firmware uses `#[cortex_m_rt::entry]` and drives
  `embassy_executor::raw::Executor` through
  `ubos_metrics::cpu_load::run_cortex_m_executor`, which brackets `wfe`
  with `Instant::now()` reads for the busy-ratio metric. bmm-pico keeps
  `#[embassy_executor::main]` because its core 1 runs a second,
  hand-managed executor for the `ubos-mining` serial worker, and it has
  no busy-ratio metric.
- The esp32s3 firmware uses `#[esp_hal::main]` and builds the esp-rtos
  executor by hand, so it can pass idle hooks to
  `Executor::run_with_callbacks`.
- Network tasks, constants, buffers and response building are shared
  through `ubos-core`, `ubos-net` and `ubos-metrics`.
- An Embassy task constructor returns `Result<SpawnToken, SpawnError>`
  and `spawner.spawn()` returns `()`, so `unwrap!` wraps the constructor.
- The chip serial link on every RP board is buffered below the task,
  by embassy-rp `BufferedUart`. The RP hardware FIFO is 32 bytes, which
  one-shot DMA overruns between reads at 921600 baud. The esp32s3 stays
  on the interrupt and FIFO path, because its 128-byte FIFO wakes on one
  byte and absorbs the jitter.
- Every dual-core board pins the `ubos-mining` `serial_task` to core 1,
  so embassy-net on core 0 stays responsive. `ASIC_SESSION` uses
  `CriticalSectionRawMutex`, so sharing it across cores costs nothing.
  On esp32s3, core 1 also runs `IdleAccounting::<1>` for the cpu-load
  metric.
- A function returns the value it produces, or `Option` or `Result` of
  it, and does not fill a caller's `&mut` argument. A `&mut` buffer is
  fine when the caller owns it for its whole lifetime and chooses its
  length, when the function transforms it in place, or when a trait or
  a HAL dictates the signature. Where a HAL dictates it, the platform
  crate wraps the call in a function that returns the value, as
  `ubos_rp::boot::flash_unique_id` does.
- A library crate's `lib.rs` holds the crate docs, the crate attributes,
  the `mod` declarations and the `pub use` lines of the public API, and
  no item of its own. Each item lives in a module named for what it is,
  so a reader finds it by the file name, and the `pub use` lines keep
  the public paths at the crate root. A module's tests live in
  `src/tests/<module>.rs`, attached with `#[path]`, and a fixture that
  several modules' tests share is a test module of its own, as in
  `ubos/transitions`. A `#[macro_export]` macro lives in its module too,
  because the attribute puts it at the crate root wherever it is
  defined. An existing crate takes this shape when its `lib.rs` is next
  edited.

## Free functions

A function at module level is correct in six cases and a method
otherwise. The line is C-METHOD of the Rust API Guidelines: the method
form needs no import at the call site, gets an automatic borrow, and
has one place in `rustdoc`. A function that gains none of the three
stays free, so `crc8(bytes)` is free, and one that threads a parameter
through many call sites becomes a method on it.

1. **Pure transform.** Primitives or wire bytes in, primitives or wire
   bytes out, no state. `str::from_utf8` is the precedent.
2. **One instance by construction.** A facade over a private `static`,
   or a hardware singleton reached through a peripheral token, such as
   a board's `init`.
3. **The receiver cannot be a local type.** A foreign type or a generic
   parameter cannot take an inherent `impl`, and an extension trait or
   a newtype each add an item.
4. **Fixed by a framework.** Entry points, interrupt handlers, tasks,
   and everything else whose shape a macro or the linker fixes.
5. **A split borrow under one lock.** The caller takes the lock once and
   hands each helper a disjoint field. A method on the handle takes the
   lock again and deadlocks.
6. **Two peer parameters.** The operation drives two values of equal
   standing, such as a chain driver and its transport.

Class 1 needs no note. Classes 2, 3, 5 and 6 are invisible from the
signature, so the reason is written down once: in the module doc when
it covers the module, on the function otherwise. A constructor is
`Type::new`, never a free `new_type()`.
