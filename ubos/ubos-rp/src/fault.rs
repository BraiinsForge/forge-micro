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
/// Spinning without feeding the watchdog lets the hardware watchdog reset the
/// board, and the breadcrumb is what separates that reset from a slow-flash
/// timeout, which sets the same TIMER reason bit. This holds only once
/// `watchdog_task` has armed the watchdog: a panic before that spins
/// forever.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    crate::ota::record_panic_breadcrumb();
    defmt::error!("firmware panic");
    loop {
        cortex_m::asm::wfi();
    }
}

/// `HardFault` handler: defmt-logs the stacked exception frame.
///
/// cortex-m-rt's default handler executes `udf`, which probe-rs reports only
/// as a bare "Exception" with an unreliable unwound backtrace. The stacked PC
/// and LR give the true faulting address, emitted over defmt-rtt before the
/// board is left to the hardware watchdog.
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
    loop {
        cortex_m::asm::wfi();
    }
}
