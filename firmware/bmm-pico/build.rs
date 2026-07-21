// Copyright (C) 2026  Braiins Systems s.r.o.
//
// All rights reserved. This file is proprietary and may not be copied,
// distributed, or disclosed except as authorized in writing.

use std::fs;
use std::path::PathBuf;

/// Build script that configures linker scripts and error handling.
///
/// For RP2350, we need memory.x copied to the linker search path plus link.x
/// and defmt.x linker scripts. Embassy-rp is built with `imagedef-none`, and
/// this application links into the ACTIVE partition rather than offset 0, so
/// it emits no image-def of its own. The reused `rp2350-bootloader` owns the
/// offset-0 image-def block the bootrom inspects and chains to ACTIVE.
/// link-rp.x (which defines `.boot2`) is not needed for RP2350.
fn main() {
    linker_be_nice();

    // Cargo always sets OUT_DIR for build scripts.
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("BUG: OUT_DIR not set during build"));

    // Copy memory.x to the output directory so the linker can find it.
    let memory_x = include_bytes!("memory.x");
    // OUT_DIR is writable by the build process.
    fs::write(out_dir.join("memory.x"), memory_x).expect("BUG: failed to write memory.x");
    println!("cargo:rustc-link-search={}", out_dir.display());

    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");

    // `config.rs` reads `UBOS_FW_VERSION` through `option_env!`; cargo does
    // not track that input, so without this an override silently reuses a
    // stale version baked into the image.
    println!("cargo:rerun-if-env-changed=UBOS_FW_VERSION");
}

// Duplicated in rp2040/build.rs, rp2350/build.rs, stm32f407/build.rs, and stm32f439/build.rs.
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
