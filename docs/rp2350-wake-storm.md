# RP2350 multicore wake storm investigation

This document records the findings and experiments performed while investigating
abnormally high CPU busy ratios / executor "empty polls" on the RP2350 build.

## Symptom

On RP2350, when using a thread-mode executor loop (`poll(); WFE`), both cores
can end up executing an extremely high number of empty `poll()` iterations
(millions per 5s window), inflating `board_cpu_busy_ratio` and wasting CPU.

## Terminology

- **Thread-mode executor**: Embassy's simplest Cortex-M executor. Wakes tasks by
  issuing `SEV` and sleeps with `WFE`.
- **InterruptExecutor**: Embassy-rp's executor variant that wakes via a per-core
  NVIC IRQ pend; off-core wakes route via SIO FIFO IPI. This avoids `SEV`-based
  cross-core wake amplification.
- **Empty poll**: An executor `poll()` iteration that finds no runnable tasks.

## Key finding 1: UART ownership is per-core NVIC state

RP2-class multicore has one NVIC per core (separate enable/pending state). For
UART interrupts to behave correctly:

- construct the UART driver on the core that polls it (core1 in our split),
- ensure the non-owning core keeps `UART1_IRQ` masked and not pending.

In `embassy-rp` 0.10.0, `BufferedUart::new()` calls `init_buffers()` which:

- enables UART interrupt mask bits (`UARTIMSC` for RX, RX timeout, TX),
- does `interrupt.unpend()` and `interrupt.enable()` (NVIC unmask) for `UART1_IRQ`.

Because NVIC operations are per-core, this configuration applies to the core
that constructs `BufferedUart`.

We validated the resulting NVIC state at runtime on both RP2040 and RP2350:

- core0: `UART1_IRQ en=false pend=false`
- core1 (after `BufferedUart::new`): `UART1_IRQ en=true pend=false`

So "UART1 enabled on the wrong core" is not the remaining explanation for the
RP2350 thread-mode poll storm when UART is constructed on core1.

## Key finding 2: Disabling Ethernet does not remove the storm

We tested a build that skipped the Wiznet + `embassy-net` initialization
entirely ("disable eth"). The empty-poll rate did not materially change.

Conclusion: the wake storm was not primarily caused by W6100 INT chatter or the
Ethernet stack.

## Key finding 3: `WFE` is the culprit on RP2350 thread-mode

We added diagnostic metrics to quantify:

- `board_cpu_poll_count`: number of executor `poll()` loop iterations per core,
  per 5s window.
- `board_wfe_isrpending_count`: number of times `WFE` returned with
  `SCB.ICSR.ISRPENDING=1` (i.e. at least one exception was pending).
- `board_wfe_lowest_pending_irq`: sampled lowest-numbered pending external IRQ
  in the NVIC at `WFE` return time (used to identify *which* IRQ line is stuck
  pending).
- `board_pender_count{kind="sev"}`: how often Embassy's `__pender` executed the
  thread-mode `SEV` path per 5s window.
- `board_embassy_irq_fire_count{kind="timer_alarm|sio_fifo|fifo_write"}`:
  coarse counts of certain embassy-rp IRQ handlers (timer alarm, SIO FIFO IRQ)
  and inter-core FIFO writes (each emits `SEV`).

Observed on RP2350 in thread-mode (`poll(); WFE`):

- `board_cpu_poll_count` was extremely high (millions per 5s window).
- `board_pender_count{kind="sev"}` was orders of magnitude lower (thousands per
  5s window).
- timer alarm IRQ rate was low (~tens per 5s), SIO FIFO ISR rate was 0, FIFO
  writes were 0.

This means the empty polls were not driven by "task wake → `__pender` → `SEV`".

Critical experiment:

- Switching the idle instruction from `WFE` to `WFI` on RP2350 immediately
  collapsed `board_cpu_poll_count` (≈ 0-100 per 5s) and `board_cpu_busy_ratio`
  became near-zero as expected for an idle system.

Conclusion: on RP2350, in our configuration, `WFE` can return continuously due
to an event source unrelated to normal task waking, causing the executor loop
to spin and inflate "busy".

### Pinpointing the event: `ISRPENDING` is always set

On RP2350 thread-mode, we observed:

- `board_wfe_isrpending_count == board_cpu_poll_count` (exact match).

That means **every single `WFE` return coincided with at least one pending
exception**. This is consistent with `WFE` returning immediately when any IRQ
becomes pending, even if that IRQ is masked/disabled on the local core.

### Which IRQs are pending?

Sampling NVIC pending state at `WFE` return time showed:

- core0: lowest pending external IRQ `34` (RP2350 table: `UART1_IRQ`)
- core1: lowest pending external IRQ `0` (RP2350 table: `TIMER0_IRQ_0`)

At the same time, the actual *handled* timer-alarm ISR rate stayed low (tens per
5s), which is only possible if these IRQs are **pending on the "non-owning"
core where they are disabled**, therefore never taken/handled.

Interpretation: RP2350 routes most peripheral interrupt lines to **both** cores'
NVICs. Enabling an IRQ on only one core (the correct pattern) still allows the
other core's NVIC to observe the line and set its pending bit. With `WFE`-based
sleep, a continuously-pending interrupt on the non-owning core causes an
immediate wake → empty `poll()` → `WFE` → immediate wake loop.

### Security state / `SEVONPEND` check

RP2350 is Cortex-M33 (Armv8-M with Security Extension). `SCB.SCR.SEVONPEND` is
banked between Secure and Non-secure state and controls whether
inactive→pending transitions generate an *event*.

We instrumented SCB/SCS bank reads and observed at boot:

- Firmware runs in **Secure state** (`secure=1`).
- `SCB.SCR.SEVONPEND=0` in both Secure and Non-secure SCR banks.
- Nevertheless, Secure `SCB.ICSR.ISRPENDING=1` while `ICSR_NS.ISRPENDING=0`.

Conclusion: the wake storm in this setup is **not** explained by `SEVONPEND`
generating events. The executor loop spins because *exceptions remain pending*
in the Secure NVIC state on the non-owning core, preventing `WFE` from sleeping
regardless of `SEVONPEND`.

### Follow-up experiments: clearing event latch and SIO IRQ state

We tested three RP2350-only mitigation/bisect experiments while staying in the
thread-mode executor model:

1. **Clear stale event latch before `WFE`**

   Use the ARM-recommended pattern `SEV; WFE; WFE` (the first `WFE` consumes the
   pending event).

   Result: this reduced the observed poll storm by about half, but did not
   eliminate it (still millions of polls per 5s window).

2. **Repeatedly `unpend()` SIO IRQs**

   On both cores, periodically call `SIO_IRQ_FIFO.unpend()` and
   `SIO_IRQ_BELL.unpend()`.

   Result: no material change to the poll storm versus (1).

3. **Repeatedly clear the SIO FIFO interrupt source (WOF)**

   Periodically clear `SIO.fifo().st().wof`.

   Result: no material change to the poll storm versus (1).

Interpretation: the storm is not explained solely by "pending-but-disabled SIO
IRQ" state. `WFE` is still returning rapidly due to an event source that is not
accounted for by timer IRQ rate, SIO FIFO ISR rate, or normal task wakes via
`__pender`.

## Key finding 4: Exclusive monitor events can wake `WFE` (ruled out here)

On Arm Cortex-M, `WFE` wakes on *events*, not just interrupts. Besides explicit
`SEV`, the Arm architecture allows the exclusive access monitor ("global
monitor") to generate an event when a core's reservation is cleared by another
master/core.

On RP2350 this matters because:

- The thread-mode executor loop performs atomics (locks, run-queue bookkeeping,
  metrics counters, etc.) before sleeping. On Cortex-M33, RMW atomics are
  typically implemented with `LDREX`/`STREX`, which create an exclusive
  reservation.
- If the core enters `WFE` with an outstanding reservation, **any cross-core
  write that clears the reservation can generate an event** and wake the
  sleeping core immediately.

This mechanism can produce extremely high "wake" rates without any matching IRQ
rate and without any calls into Embassy's `__pender`.

### Proof/mitigation attempt: `CLREX` before `WFE`

The simplest way to test this hypothesis is to clear the reservation just
before sleeping:

- enable feature `cpu-load-wfe-clrex` (adds `CLREX` right before `WFE` in the
  Cortex-M executor loop).
- observe whether `board_cpu_poll_count` collapses similarly to the `WFI`
  variant.

Result on our RP2350 build: **no collapse**. Poll counts remained in the same
multi-million-per-window range.

Conclusion: exclusive-monitor events are *not* the dominant source of the wake
storm in this firmware/configuration.

## Practical outcome

- RP2350 uses `embassy_rp::executor::InterruptExecutor` so executor waking is
  per-core and not `SEV`/`WFE` based.
- RP2040 continues to use the thread-mode executor loop; its wake/poll rate is
  much lower in practice.

## RP2040 comparison (why it "just works")

Running the same `WFE` diagnostics on RP2040 shows a qualitatively
different regime:

- `board_cpu_poll_count` is low (~10-15k per 5s window per core), matching the
  expected interrupt cadence + real wakeups.
- `board_wfe_isrpending_count` matches `board_cpu_poll_count` (so `WFE` returns
  when some exception is pending), but **the pending condition does not stay
  latched forever**.
- Sampled `board_wfe_lowest_pending_irq` points at ordinary lines:
  - core0: IRQ `6` (`XIP_IRQ` in the RP2040 vector table ordering used by
    `embassy-rp`)
  - core1: IRQ `0` (`TIMER_IRQ_0`)

Interpretation: on RP2040, the thread-mode `poll(); WFE` loop behaves as
intended because the set of pending exceptions is transient and tied to real
interrupt delivery/handling. On RP2350, we observed continuously pending IRQ
lines on the non-owning core (disabled/masked there), making `WFE` return
immediately and spin.

### NVIC dumps (ground truth)

We added a `diag-nvic-dump` task that prints NVIC pending/enable bitsets via RTT
every 2 seconds on each core.

RP2350 (showing first 128 IRQs as `ISPR[0..3]` / `ISER[0..3]`):

- core1: `ISPR0` repeatedly shows bit0 set (`TIMER0_IRQ_0`) and `ISPR1` shows
  bit2 set (`UART1_IRQ` = IRQ 34). Both are **disabled** on core1 (owning cores
  differ), so they remain pending there.
- core0: `ISPR1` shows bit2 set (`UART1_IRQ` pending) while UART1 is owned by
  core1.

RP2040 (single `ISPR0`/`ISER0` word for 32 IRQs):

- Both cores show some pending bits (including `TIMER_IRQ_0` on the non-owning
  core and SIO IRQs), but this does not lead to a `WFE` spin storm in practice.

These dumps confirm the key difference is not "does any IRQ ever become pending
on the non-owning core" (it can), but that on RP2350 the specific high-activity
lines (`UART1_IRQ`, `TIMER0_IRQ_0`) end up stuck pending on the non-owning core,
which breaks the thread-mode `WFE` sleep loop.

### Does `WFE` actually sleep?

We added a diagnostic that measures the time spent inside `WFE` per loop:

- `board_wfe_sleep_ticks`: sum of `Instant` ticks spent inside `WFE` over the 5s window.
- `board_wfe_immediate_returns`: number of `WFE` calls that returned in ≤ 1 tick.

Results:

- RP2350: `board_wfe_immediate_returns ≈ board_cpu_poll_count` (almost every
  `WFE` returns immediately), and total `board_wfe_sleep_ticks` is tiny.
- RP2040: `board_wfe_immediate_returns` is much smaller than `board_cpu_poll_count`
  (most `WFE` calls actually sleep), and `board_wfe_sleep_ticks` is large.

This is the direct, quantitative explanation for "RP2040 works but RP2350 spins":
RP2350 is not sleeping in `WFE`; RP2040 is.

## Notes / follow-ups

If we want to keep thread-mode on RP2350, a mitigation is to use `WFI` instead
of `WFE` in the idle path. However, the robust solution remains
`InterruptExecutor` on multicore because it avoids cross-core wake amplification
and is resilient to `WFE` event behavior.

## Conceptual background: `WFE` events vs `WFI` interrupts

`WFE` ("wait for event") and `WFI` ("wait for interrupt") are often conflated,
but they have different wake models:

- `WFI` sleeps until an interrupt is *taken* on the local core (enabled and of
  sufficient priority). It is generally a good "idle until real work arrives"
  primitive.
- `WFE` sleeps until the core's *event register* is set. `SEV` explicitly sets
  that register, and many multicore systems use `SEV` for cross-core signaling.

Embassy's **thread-mode executor** uses exactly this model: waking a task issues
`SEV`, and the executor idle loop uses `WFE`. This is an intended use of `WFE`
on Arm systems, especially for cross-core wakeups. See for example:

- <https://s-o-c.org/arm-sleep-mode-entry-and-exit-differences-wfe-vs-wfi/>
- Arm Cortex-M `WFE/WFI` behavior, pending interrupts, and `SCR.SEVONPEND`:
  - RP2350 datasheet notes cross-core events and a "sticky" event flag for `SEV`
    and `WFE` on the Arm core:
    - <https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf>
  - Arm's Cortex-M33 Devices Generic User Guide covers `WFE/WFI` and
    `SCB.SCR.SEVONPEND` behavior:
    - <https://developer.arm.com/documentation/100235/latest/>
  - Arm Community discussion of `WFE/WFI` and pending interrupts:
    - <https://community.arm.com/support-forums/f/architectures-and-processors-forum/4039/wfe-wfi-and-pending-interrupts/10463>
  - `WFE` operation summary (event register semantics, `SEVONPEND` wake-on-pend):
    - <https://onlinedocs.microchip.com/oxy/GUID-199548F4-607C-436B-80C7-E4F280C1CAD2-en-US-1/GUID-E085F3C5-CD81-4673-8CD6-06A9E689F100.html>
  - Generic note: "an interrupt can enter pending state even if it is disabled"
    (disabling prevents taking it, not pending it):
    - <https://onlinedocs.microchip.com/oxy/GUID-199548F4-607C-436B-80C7-E4F280C1CAD2-en-US-1/GUID-6AE70B82-AE5F-47C3-9662-7AC67A882BA0.html>
- Embassy executor docs (thread-mode executor uses `WFE/SEV`, interrupt-mode
  executor uses an IRQ pend):
  - <https://docs.embassy.dev/>
- Background reading on wake-up interrupt controllers / wake sources (broad,
  not Cortex-M-specific):
  - <https://www.sciencedirect.com/topics/engineering/wake-up-interrupt-controller>

However, `WFE` is only suitable for executor idle when event and pending
conditions are well-behaved. On RP2350, we measured that `WFE` returns
immediately almost every time in the dual-core thread-mode configuration. In
that regime, `WFE` no longer provides an "idle sleep" and instead produces a
busy polling loop.

The `InterruptExecutor` avoids this failure mode by turning task wakeups into a
local NVIC interrupt pend (and on RP2350 routing off-core wakes via SIO FIFO),
allowing main thread idle to be a simple `WFI`.

## Public parallels and practical mitigations

This "wake storm" pattern (idle loop uses `WFE`, but the core never sleeps due
to a latched event or perpetually-pending exception/IRQ) is a known class of
issues on Cortex-M systems:

- **A disabled IRQ can still be pending**, and pending exceptions can make `WFE`
  return immediately, depending on configuration and which wake sources set the
  event register. (See the references above.)

Common mitigations that map directly to what we observed here:

- Prefer **`WFI` for idle** when you expect "masked-but-pending" IRQs to exist on
  a core (e.g., multicore systems where peripheral lines can appear on both
  cores' NVICs, but are only enabled/handled on one core).
- If you must use `WFE`, use the ARM-recommended **`SEV; WFE; WFE`** sequence to
  clear a stale event latch before sleeping (helps if the event register is the
  only reason `WFE` is returning immediately).
- Eliminate the root cause: ensure no **level-sensitive interrupt source** stays
  asserted/pending on a core that will never service it (clear at the peripheral
  and/or avoid routing/observing that line on the non-owning core, where
  possible).

## Debug/trace notes (SWD, SWO/ITM, ETM)

For this class of timing-sensitive "wake storm" problems:

- **SWD debugging (halt/breakpoints/watchpoints)** is useful to inspect state,
  but halting the core perturbs interrupts and event delivery, and can mask the
  problem.
- **SWO/ITM tracing** can attribute high-rate events without halting, but it
  requires the target's SWO pin to be physically routed and a probe that
  supports SWO. Using a second Pico as the probe generally does not give you
  SWO in a convenient/robust way.
- **ETM (Embedded Trace Macrocell)** is ARM's full instruction trace hardware.
  When available, it can stream a compressed trace of executed instructions in
  real time. It requires dedicated trace pins (TPIU trace port) and a probe
  that supports ETM trace capture. Typical Pico-based probe setups and most RP
  carrier boards do not route these pins, so ETM is usually not an option here.

Given the above constraints, the most reliable approach for this repo is the
one used throughout this investigation: defmt/RTT plus low-overhead counters
exported via Prometheus.
