/// Build script that configures linker scripts and error handling.
///
/// For STM32, the memory-x feature of embassy-stm32 auto-generates the memory
/// layout. We only need link.x and defmt.x linker scripts.
fn main() {
    linker_be_nice();

    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    println!("cargo:rerun-if-changed=build.rs");
}

// Duplicated in rp2040/build.rs. Build scripts cannot share code
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
