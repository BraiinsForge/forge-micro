// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! The panic handler and the `HardFault` exception, emitted here as library
//! symbols. Each is once per binary, so a board that keeps its own copy fails
//! the link with `symbol multiply defined!`.

/// Custom panic handler: records a breadcrumb in SCRATCH2 and spins.
///
/// The breadcrumb distinguishes a panic-induced watchdog reset from a
/// slow-flash watchdog timeout: both set the watchdog TIMER reason bit,
/// but only a panic sets SCRATCH2 to `WRITE_PHASE_PANIC`. The probe is
/// detached during OTA uploads so defmt output is not observed; the
/// breadcrumb is the only durable record that survives the warm reset.
/// Spinning without feeding the watchdog causes the 2 s hardware watchdog
/// to fire, which resets the board with the TIMER reason bit set, and
/// SCRATCH2 carries the panic marker to the next boot. This holds only
/// once `watchdog_task` has armed the watchdog; a panic in early boot
/// before arming spins forever, the same window the prior `panic_probe`
/// handler had.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    ubos_ota::record_panic_breadcrumb();
    defmt::error!("firmware panic");
    // Spin without feeding the watchdog so the hardware watchdog fires and
    // resets the board, leaving SCRATCH2 set for the next boot to read.
    loop {
        cortex_m::asm::wfi();
    }
}

/// `HardFault` handler: defmt-logs the stacked exception frame.
///
/// A `HardFault` is distinct from a Rust panic and from a watchdog
/// timeout. cortex-m-rt's default handler executes `udf`, which probe-rs
/// reports only as a bare "Exception" with an unreliable unwound
/// backtrace. Logging the stacked PC and LR gives the true faulting
/// address. The frame is emitted over defmt-rtt before the board is
/// left to the hardware watchdog, so `cargo run` captures it even when
/// the unwinder cannot.
#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    defmt::error!(
        "HardFault: pc={=u32:#x} lr={=u32:#x} xpsr={=u32:#x} r0={=u32:#x} r1={=u32:#x} r2={=u32:#x} r3={=u32:#x} r12={=u32:#x}",
        frame.pc(),
        frame.lr(),
        frame.xpsr(),
        frame.r0(),
        frame.r1(),
        frame.r2(),
        frame.r3(),
        frame.r12(),
    );
    // Spin without feeding the watchdog so the hardware watchdog resets the
    // board after the frame has been emitted.
    loop {
        cortex_m::asm::wfi();
    }
}
