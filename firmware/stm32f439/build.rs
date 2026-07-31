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

/// Build script that configures linker scripts and error handling.
///
/// The local `memory.x` is copied into `$OUT_DIR` and that directory is
/// added to the linker search path so cortex-m-rt's `link.x` picks up
/// our extended layout instead of the one embassy-stm32 would generate.
/// We carry our own memory.x to declare the CCM RAM region; the
/// `memory-x` feature on the `embassy-stm32` dependency is therefore
/// disabled.
fn main() {
    linker_be_nice();

    let out_dir = std::env::var_os("OUT_DIR").expect("BUG: OUT_DIR not set by cargo");
    let destination = std::path::Path::new(&out_dir).join("memory.x");
    std::fs::copy("memory.x", &destination).expect("BUG: copy memory.x to OUT_DIR");
    println!(
        "cargo:rustc-link-search={}",
        std::path::Path::new(&out_dir).display()
    );

    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}

// Duplicated in bmm-pico/build.rs, rp2040/build.rs, rp2350/build.rs, and stm32f407/build.rs.
// Build scripts cannot share code without a build-dependency crate, so the duplication is
// intentional.
fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 2 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                "_defmt_timestamp" => {
                    eprintln!();
                    eprintln!(
                        "defmt not found - make sure defmt.x is added as a \
                         linker script and you have included use defmt_rtt as _;"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("Is the linker script link.x missing?");
                    eprintln!();
                }
                _ => (),
            },
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    // The build script binary exists at a known path during execution.
    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe()
            .expect("BUG: failed to get build script path")
            .display()
    );
}
