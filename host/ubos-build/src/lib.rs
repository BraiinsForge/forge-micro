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
/// Call it first in a board's `main`, before the build id is stamped:
/// `linker_be_nice` re-enters the build script binary as the linker's
/// error-handling script and exits from there, so whatever a board does
/// before the call also runs on that path.
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
/// `build.rs` as well, because the first such directive replaces cargo's
/// default of re-running on any change in the package.
///
/// # Panics
///
/// Panics when `OUT_DIR` is unset or the copy fails, both of which are
/// cargo's side of the contract rather than anything the board can
/// handle.
pub fn copy_memory_x() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("BUG: OUT_DIR not set by cargo"));
    fs::copy(MEMORY_X, out_dir.join(MEMORY_X)).expect("BUG: failed to copy memory.x to OUT_DIR");
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rerun-if-changed={MEMORY_X}");
}

/// The variables that hand a build the parts of its id, for a build with
/// no `.git` to read. The nix image builds set them from the flake's
/// revision, its dirty state and its last-modified date. A non-empty dirty
/// variable marks a dirty tree.
const BUILD_COMMIT_ENV: &str = "UBOS_BUILD_COMMIT";
const BUILD_DIRTY_ENV: &str = "UBOS_BUILD_DIRTY";
const BUILD_STAMP_ENV: &str = "UBOS_BUILD_STAMP";

/// The longest commit or stamp an override may hold. The metrics store
/// carries both as label values, which bound them.
const MAX_PART_LEN: usize = 12;

/// The directories of this tree, relative to its root, whose sources reach
/// a board image. A change under any of them re-runs the board's build
/// script and moves the build id. Every entry must name a directory that
/// exists, because cargo answers a named path that is absent by re-running
/// the script on every build.
const OPEN_SOURCE_DIRS: [&str; 6] = ["brand", "firmware", "host", "micro-sv2", "patches", "ubos"];

/// Stamp the build id of a board crate in this tree.
///
/// Call it from the board's `build.rs`. Every board crate sits two levels
/// below the root of this tree, so the source directories resolve from the
/// crate directory and a crate at another depth would name absent paths.
///
/// # Panics
///
/// When `CARGO_MANIFEST_DIR` is unset, which cargo guarantees for a build
/// script it runs.
pub fn open_board_build_id() {
    let dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("BUG: CARGO_MANIFEST_DIR not set by cargo");
    let root = Path::new(&dir).join("../..");
    let sources: Vec<PathBuf> = OPEN_SOURCE_DIRS.iter().map(|sub| root.join(sub)).collect();
    build_id(&sources);
}

/// Stamp a build id into the `BUILD_ID` variable the boot banner, the REST
/// API and the package name read, and its two parts into `BUILD_COMMIT`
/// and `BUILD_STAMP`, which the metrics read as labels. The id is the tip
/// of the whole checkout and the UTC time this script last ran,
/// `<hash>[-dirty]-<yymmddHHMMSS>`. The tip is the superproject's when the
/// tree is a submodule of one, because that commit names every source the
/// build used. [`BUILD_COMMIT_ENV`] and [`BUILD_STAMP_ENV`] replace the two
/// parts and must be set together, and [`BUILD_DIRTY_ENV`] replaces the
/// dirty marker.
///
/// Cargo re-runs a build script only for the inputs it names, so `sources`
/// must name every directory whose sources reach the image, and the log of
/// the tip's `HEAD` is named too because a commit moves the tip without
/// touching a source.
///
/// # Panics
///
/// When only one of [`BUILD_COMMIT_ENV`] and [`BUILD_STAMP_ENV`] is set,
/// or when either holds more than 12 bytes.
pub fn build_id<P: AsRef<Path>>(sources: &[P]) {
    for var in [BUILD_COMMIT_ENV, BUILD_DIRTY_ENV, BUILD_STAMP_ENV] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let set = |var| std::env::var(var).ok().filter(|value| !value.is_empty());
    match (set(BUILD_COMMIT_ENV), set(BUILD_STAMP_ENV)) {
        (Some(commit), Some(stamp)) => {
            assert!(
                commit.len() <= MAX_PART_LEN && stamp.len() <= MAX_PART_LEN,
                "{BUILD_COMMIT_ENV} and {BUILD_STAMP_ENV} hold at most {MAX_PART_LEN} bytes each"
            );
            emit(&commit, set(BUILD_DIRTY_ENV).is_some(), &stamp);
            return;
        }
        (None, None) => {}
        _ => panic!("set both {BUILD_COMMIT_ENV} and {BUILD_STAMP_ENV}, or neither"),
    }
    for dir in sources {
        println!("cargo:rerun-if-changed={}", dir.as_ref().display());
    }
    let tip = run(
        None,
        "git",
        &["rev-parse", "--show-superproject-working-tree"],
    )
    .or_else(|| run(None, "git", &["rev-parse", "--show-toplevel"]));
    let tip = tip.as_deref().map(Path::new);
    if let Some(head_log) = run(
        tip,
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
    let hash =
        run(tip, "git", &["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "nogit".into());
    let dirty = run(
        tip,
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some();
    let stamp =
        run(None, "date", &["-u", "+%y%m%d%H%M%S"]).unwrap_or_else(|| "000000000000".into());
    emit(&hash, dirty, &stamp);
}

/// Write the cargo directives that stamp the build id and its two parts.
/// The id keeps the dirty marker, and the parts do not carry it.
fn emit(commit: &str, dirty: bool, stamp: &str) {
    for directive in directives(commit, dirty, stamp) {
        println!("{directive}");
    }
}

/// The cargo directives for the parts of a build id.
fn directives(commit: &str, dirty: bool, stamp: &str) -> [String; 3] {
    let marker = if dirty { "-dirty" } else { "" };
    [
        format!("cargo:rustc-env=BUILD_COMMIT={commit}"),
        format!("cargo:rustc-env=BUILD_STAMP={stamp}"),
        format!("cargo:rustc-env=BUILD_ID={commit}{marker}-{stamp}"),
    ]
}

/// Run a command in `dir`, or in the build script's own directory when it
/// is `None`, and return its trimmed stdout. A failure and an empty output
/// both give `None`.
fn run(dir: Option<&Path>, cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(cmd);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let out = command.output().ok()?;
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
/// The linker passes the failure kind and the symbol as arguments, and
/// the re-entered binary exits before cargo reads any directive from it.
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

#[cfg(test)]
mod tests {
    use super::directives;

    /// The commit is 12 characters and the stamp is 12 digits, and the id
    /// is built from the two with the dirty marker between them.
    #[test]
    fn parts_and_id_agree() {
        let [commit, stamp, id] = directives("0123456789ab", true, "261127101500");
        assert_eq!(commit, "cargo:rustc-env=BUILD_COMMIT=0123456789ab");
        assert_eq!(stamp, "cargo:rustc-env=BUILD_STAMP=261127101500");
        assert_eq!(
            id,
            "cargo:rustc-env=BUILD_ID=0123456789ab-dirty-261127101500"
        );
        let [_, _, clean] = directives("0123456789ab", false, "261127101500");
        assert_eq!(clean, "cargo:rustc-env=BUILD_ID=0123456789ab-261127101500");
    }
}
