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

//! Pre-init helper for zeroing `.ccmram.bss`.
//!
//! cortex-m-rt's startup only zeroes the standard `.bss`. Boards that
//! relocate statics into Core-Coupled Memory via the `ccmram-bss`
//! feature need to zero that region themselves before any of those
//! statics are read. The logic is identical on every CCM-equipped
//! board, so the loop lives here and each firmware calls it from a
//! one-line `#[cortex_m_rt::pre_init]`.
//!
//! # Type invariant for CCM-placed statics
//!
//! Anything placed in `.ccmram.bss` must be **bit-equivalent to an
//! all-zero memory image after `Type::new()`**. The section is zeroed
//! in pre-init and then handed out as a fully-initialised static; if
//! the type's `new()` value has any non-zero discriminant, niche, or
//! flag byte, the static is observed in an undefined state.
//!
//! Types known to satisfy this on 32-bit ARM include the heapless
//! `FnvIndexMap` used by `ubos-metrics` (`indices: [Option<Pos>; N]`
//! where `None` is the zero niche of `NonZeroU32`; in heapless 0.9
//! `VecInner` declares `len` before `buf`, so `len` is at offset zero
//! past the zero-sized `PhantomData` field and starts at zero) and the
//! `Mutex<_, FnvIndexMap<_>>`
//! / `Mutex<_, Option<_>>` wrappers used by `ubos-api` (the inner
//! atomic state's zero value means "unlocked"), and the
//! `edge_nal_embassy::Pool<T, N>` (aliased as `TcpBuffers`) used by the
//! REST and webui listeners (`used: [Cell<bool>; N]` is `false` at zero
//! and `data: [UnsafeCell<MaybeUninit<T>>; N]` stays uninhabited until
//! the pool hands out a slot, so the all-zero image is the same valid
//! "empty pool" state that `Pool::new()` constructs).
//!
//! The `ubos-mining` mailbox statics relocated on stm32f407
//! (`STATE_WATCH`, `COMMAND_CHANNEL`, `STOP_MINING`) add the
//! embassy-sync 0.6.2 async primitives `Signal`, `Watch`, and
//! `Channel`. Each is bit-equivalent to all-zero after its `new()`:
//! `Signal::new()` is the `State::None` discriminant 0, and `Watch`
//! and `Channel` zero to `Option::None`, an empty `Deque`, a
//! `WakerRegistration::None`, and a len-0 `heapless::Vec`. Anything
//! not on this list MUST be audited before it gains
//! `link_section = ".ccmram.bss"`, since the compiler will not catch a
//! mismatch.
//!
//! The per-chain statics stay out on purpose. `ubos-metrics`'s
//! `CHAIN_STATS` starts its hottest-die field at the `NO_TEMPERATURE`
//! sentinel, which is not zero, and each board's `CHAINS` array of
//! `Serial` stores the chain's own index, which is zero only for the
//! first chain. Zeroing either at boot would silently corrupt them.
//!
//! The Rust language does not guarantee that `Option<T>::None` is
//! represented as all-zero bytes for an arbitrary `T`; that depends on
//! whether `T` has a usable niche. Only types whose representation is
//! audited at the source level may appear here. A compiler upgrade
//! that adds a niche to an existing type, or a field addition that
//! introduces a non-zero default, can silently invalidate this list,
//! so the audit MUST be re-run when the inner type changes.

/// Emit a `#[cortex_m_rt::pre_init]` function that zeroes the
/// `.ccmram.bss` linker section before main runs.
///
/// Expanded at the crate root of a firmware binary so the
/// `cortex_m_rt::pre_init` attribute lands where the linker expects
/// it. `cortex-m-rt` must be in the firmware's dependencies under
/// its conventional crate name.
#[macro_export]
macro_rules! pre_init_zero_ccmram_bss {
    () => {
        #[::cortex_m_rt::pre_init]
        unsafe fn __ubos_core_pre_init_zero_ccmram_bss() {
            // SAFETY: cortex_m_rt::pre_init runs this function exactly
            // once, before any other code observes `.ccmram.bss`.
            unsafe { $crate::ccmram::zero_bss_section() };
        }
    };
}

/// Zero the `.ccmram.bss` linker section.
///
/// # Safety
///
/// Must be called exactly once, from a `#[cortex_m_rt::pre_init]`
/// function, before any static placed in `.ccmram.bss` is observed.
/// The caller's linker script must define `__sccmram_bss` and
/// `__eccmram_bss` boundary symbols around the section.
pub unsafe fn zero_bss_section() {
    unsafe extern "C" {
        static mut __sccmram_bss: u32;
        static mut __eccmram_bss: u32;
    }
    let start = core::ptr::addr_of_mut!(__sccmram_bss);
    let end = core::ptr::addr_of_mut!(__eccmram_bss);
    // `pointer::offset_from` requires both pointers to share an allocated
    // object; the boundary symbols are separate `extern "C"` statics
    // tied to linker symbols, so derive word counts from raw addresses
    // to stay inside the abstract machine.
    let words = (end as usize).saturating_sub(start as usize) / core::mem::size_of::<u32>();
    for index in 0..words {
        // SAFETY: `words` is the byte distance from `__sccmram_bss` to
        // `__eccmram_bss` divided by `size_of::<u32>()`, so
        // `start.add(index)` stays inside the linker-defined section.
        unsafe { start.add(index).write_volatile(0) };
    }
}
