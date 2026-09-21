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

//! Build-script support shared by the board crates.
//!
//! A build script cannot reach another crate's code without a build
//! dependency, which is why every board carried its own copy of the
//! linker wiring until this crate existed. Each function here is called
//! from a board's `build.rs` and writes cargo directives to stdout.
//!
//! Cargo runs a build script with the crate's manifest directory as the
//! working directory, so a relative path here resolves against the board.
//! That is what lets the file copy live in a library at all:
//! `include_bytes!` resolves against this source file instead, so it
//! cannot move out of the board.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The linker layout every board keeps beside its manifest.
const MEMORY_X: &str = "memory.x";

/// Wire up the linker for a cortex-m board.
///
/// This is the whole body of a board's `main`. Call it first and as the
/// only statement: `linker_be_nice` re-enters the build script binary as
/// the linker's error-handling script and exits from there, so whatever a
/// board does before the call also runs on that path.
///
/// # Panics
///
/// Panics for the reasons [`copy_memory_x`] gives.
pub fn cortex_m_board() {
    linker_be_nice();
    copy_memory_x();

    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    println!("cargo:rerun-if-changed=build.rs");

    // `config.rs` reads `UBOS_FW_VERSION` through `option_env!`; cargo does
    // not track that input, so without this an override silently reuses a
    // stale version baked into the image.
    println!("cargo:rerun-if-env-changed=UBOS_FW_VERSION");
}

/// Copy the board's `memory.x` into `$OUT_DIR` and put that directory on
/// the linker search path, so `link.x` finds the board's layout rather
/// than the one its HAL would supply.
///
/// A caller that emits any other `rerun-if-changed` directive must name
/// `build.rs` as well. The first such directive replaces cargo's default
/// of re-running on any change in the package, so from then on the script
/// re-runs only for the paths it names.
///
/// # Panics
///
/// Panics when `OUT_DIR` is unset or the copy fails. Both are cargo's side
/// of the contract, so a failure here is a broken build environment rather
/// than anything the board can handle.
pub fn copy_memory_x() {
    // Cargo always sets OUT_DIR for build scripts, and OUT_DIR is writable.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("BUG: OUT_DIR not set by cargo"));
    fs::copy(MEMORY_X, out_dir.join(MEMORY_X)).expect("BUG: failed to copy memory.x to OUT_DIR");
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rerun-if-changed={MEMORY_X}");
}

/// Stamp a build id into the `BUILD_ID` variable the boot banner reads.
///
/// No board in this repository calls this. The callers are boards in the
/// internal repository that consumes these crates, and their banners read
/// `BUILD_ID` through `env!`.
///
/// The id is `git describe` plus the wall clock at the moment this script
/// last ran, so two builds from the same commit stay distinguishable and
/// the suffix reads as the time of the last build. Cargo re-runs a build
/// script only for the inputs it names, and an edit to a shared crate
/// relinks the board without touching the board's own sources, so the
/// caller names every directory whose sources reach the image. Cargo
/// scans a named directory for any change, so the stamp moves when a
/// source moves and stays when nothing did; a script that re-ran on every
/// build recompiled the board on every run with nothing changed. The
/// commit part has one input more, the history itself, which a commit
/// moves without touching a source file, so the log of `HEAD` is named
/// too. A tag is the one move that log does not record: a tag set on an
/// already-built commit changes what `git describe` reports without
/// moving `HEAD`, so the stamp keeps the old name until the next source
/// change re-runs the script. The tag refs are not named because they
/// are packed on most clones, and a named path that is absent makes
/// cargo re-run on every build, which is the fault this design removes.
pub fn build_id<P: AsRef<Path>>(sources: &[P]) {
    for dir in sources {
        println!("cargo:rerun-if-changed={}", dir.as_ref().display());
    }
    if let Some(head_log) = run(
        "git",
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "logs/HEAD",
        ],
    ) {
        println!("cargo:rerun-if-changed={head_log}");
    }
    let describe = run("git", &["describe", "--tags", "--always", "--dirty"])
        .unwrap_or_else(|| "nogit".into());
    let stamp = run("date", &["+%y%m%d%H%M%S"]).unwrap_or_else(|| "000000000000".into());
    println!("cargo:rustc-env=BUILD_ID={describe}-{stamp}");
}

/// Run a command and return its trimmed stdout, or `None` if it fails.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// Install this build script as the linker's error-handling script, and
/// serve that role when the linker calls back into it.
///
/// The linker passes the failure kind and the symbol as arguments, so the
/// re-entered binary translates the two undefined symbols a missing linker
/// script produces into an explanation, then exits before cargo reads any
/// directive from it.
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
