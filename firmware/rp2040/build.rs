use std::fs;
use std::path::PathBuf;

/// Build script that configures linker scripts and error handling.
///
/// For RP2040, we need memory.x copied to the linker search path plus link.x,
/// link-rp.x, and defmt.x linker scripts. The error handling script provides
/// helpful diagnostics when the linker encounters undefined symbols.
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
    println!("cargo:rustc-link-arg=-Tlink-rp.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}

// Duplicated in stm32f407/build.rs. Build scripts cannot share code
// without a build-dependency crate, so the duplication is intentional.
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
