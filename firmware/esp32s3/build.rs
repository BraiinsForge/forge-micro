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

use std::fs;
use std::path::PathBuf;

/// Build script that adds a link-time guard on the core-0 stack.
///
/// esp-hal lays `.data`, `.bss`, and the core-0 stack into the same
/// `dram_seg`: the stack starts at the top of the region and grows down
/// toward `.bss`. There is no fixed stack size; whatever `.bss` does not
/// occupy is the stack. Cumulative `.bss` growth therefore silently shrinks
/// the boot stack, and once it falls below the peak `embassy_net::new` needs,
/// the stack grows past `__stack_chk_guard` and esp-hal's stack-guard
/// watchpoint halts the core. That failure only shows up on hardware, late.
///
/// `stack-floor.x` turns it into a build error instead: it asserts the free
/// stack between `_bss_end` and `_stack_start_cpu0` stays above a floor. Both
/// symbols come from esp-hal's `linkall.x`; the assert sits outside any
/// SECTIONS block, so the linker evaluates it after final allocation and the
/// fragment only adds the check without replacing the layout script.
fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("BUG: OUT_DIR not set by cargo"));

    // 24 KiB floor. The boot path overran the historical ~8 KiB stack inside
    // `embassy_net::new`; this keeps a 3x margin over that observed failure.
    let stack_floor = "ASSERT(_stack_start_cpu0 - _bss_end >= 0x6000, \
        \"core-0 free stack below 24 KiB: .bss has crowded dram_seg; \
        shrink static buffers (e.g. the HTTP pools in config.rs) or the boot \
        stack will overflow into __stack_chk_guard during embassy_net::new\")\n";
    fs::write(out_dir.join("stack-floor.x"), stack_floor)
        .expect("BUG: failed to write stack-floor.x");
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-arg=-Tstack-floor.x");

    println!("cargo:rerun-if-changed=build.rs");

    // `config.rs` reads `UBOS_FW_VERSION` through `option_env!`; cargo does
    // not track that input, so without this an override silently reuses a
    // stale version baked into the image.
    println!("cargo:rerun-if-env-changed=UBOS_FW_VERSION");
}
