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

//! Pre-compress the Preact dashboard and login pages for the firmware.
//!
//! The frontend is a Preact + Rsbuild project under `web/` with two HTML
//! entries (`index.html`, `login.html`). `rsbuild build` emits one
//! self-contained, minified HTML file per entry (all JS and CSS
//! inlined). This script obtains those two files and gzips each into
//! `OUT_DIR/<name>.html.gz`, which `src/lib.rs` embeds via
//! `include_bytes!` and `dashboard.rs` serves with `Content-Encoding:
//! gzip`, so first paint costs no runtime compression on the MCU.
//! Gzip rather than Brotli: browsers only advertise `br` in
//! `Accept-Encoding` over HTTPS and on `localhost`, and the board is
//! reached over plain HTTP by IP, so a Brotli response would never be
//! requested. Zopfli is an exhaustive DEFLATE encoder whose output is
//! a standard gzip stream a few percent smaller than flate2 level 9;
//! build time does not matter here, embedded bytes do.
//!
//! Two ways to obtain the built pages:
//!   * If `UBOS_WEBUI_DIST` is set, read `<dist>/index.html` and
//!     `<dist>/login.html`. The flake sets this to a hermetic
//!     `buildNpmPackage` derivation output, so the Rust check sandbox
//!     needs no Node.
//!   * Otherwise (a dev shell with Node on PATH) run `npm run build`
//!     in the crate directory and read the resulting `dist/`.
//!
//! Build provenance (`UBOS_WEBUI_BUILD_REV`, `UBOS_WEBUI_BUILD_TIME`)
//! is stamped here, not in the bundle: `rsbuild.config.ts` defines the
//! footer's revision and time as the two tokens below, and this script
//! replaces them in each page before compressing it. So the built dist
//! is the same for every commit and is cached across them, while the
//! pages a firmware embeds still carry the revision that built them.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use std::num::NonZeroU64;

/// Zopfli iteration count. The upstream binary's `--i1000` setting for
/// maximum compression; gains flatten well before this, and the inputs
/// are small enough that build time stays in seconds.
const ZOPFLI_ITERATIONS: u64 = 1000;

/// Gzip `bytes` with Zopfli at maximum effort and write to `dest`.
fn gzip_to(bytes: &[u8], dest: &Path) {
    let options = zopfli::Options {
        iteration_count: NonZeroU64::new(ZOPFLI_ITERATIONS).expect("BUG: iteration count is zero"),
        ..Default::default()
    };
    let mut compressed = Vec::new();
    zopfli::compress(options, zopfli::Format::Gzip, bytes, &mut compressed)
        .expect("BUG: zopfli encoder rejected HTML bytes");
    fs::write(dest, compressed).unwrap_or_else(|_| panic!("BUG: write {}", dest.display()));
}

/// The tokens `rsbuild.config.ts` defines the footer's revision and
/// time as, replaced by [`stamp`] in each built page.
const REV_TOKEN: &str = "@UBOS_WEBUI_BUILD_REV@";
const TIME_TOKEN: &str = "@UBOS_WEBUI_BUILD_TIME@";

/// The built pages, each beside the provenance tokens it must carry.
/// Only the dashboard shows them, in its footer, so the login page
/// names none. A page that is missing a token it names was built from a
/// config that no longer matches this script, so that is a bug rather
/// than a page to ship unstamped.
const PAGES: [(&str, &[&str]); 2] = [("index", &[REV_TOKEN, TIME_TOKEN]), ("login", &[])];

/// Replace the provenance tokens in a built page. A page that carries
/// neither token comes back unchanged.
fn stamp(html: &str, build_rev: &str, build_time: &str) -> String {
    html.replace(REV_TOKEN, build_rev)
        .replace(TIME_TOKEN, build_time)
}

/// Run `npm run build` in the crate directory. Installs dependencies
/// first when `node_modules` is absent so a fresh checkout builds
/// without a manual `npm ci`.
fn run_web_build(manifest_dir: &Path) {
    // `npm` is on PATH in the Nix dev shells that add `nodejs`. This
    // path is never taken in the flake checks: those set
    // `UBOS_WEBUI_DIST` to a prebuilt hermetic dist instead.
    if !manifest_dir.join("node_modules").exists() {
        run_npm(manifest_dir, &["ci"]);
    }
    run_npm(manifest_dir, &["run", "build"]);
}

fn run_npm(manifest_dir: &Path, args: &[&str]) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(manifest_dir)
        .status()
        .unwrap_or_else(|_| {
            panic!(
                "BUG: npm missing from PATH; enter a nix devshell that provides \
                 `nodejs`, or set UBOS_WEBUI_DIST to a prebuilt dist"
            )
        });
    assert!(status.success(), "npm {args:?} exited {status:?}");
}

fn main() {
    // OUT_DIR is set by cargo for every build script invocation.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR unset by cargo"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("BUG: CARGO_MANIFEST_DIR unset"));

    let build_rev = resolve_build_rev(&manifest_dir);
    let build_time = resolve_build_time();

    // Locate the built pages: a prebuilt hermetic dist wins; otherwise
    // build it here with Rsbuild.
    let dist_dir = if let Some(dir) = env::var_os("UBOS_WEBUI_DIST") {
        PathBuf::from(dir)
    } else {
        run_web_build(&manifest_dir);
        manifest_dir.join("dist")
    };

    for (name, tokens) in PAGES {
        let src = dist_dir.join(format!("{name}.html"));
        let html = fs::read_to_string(&src)
            .unwrap_or_else(|_| panic!("BUG: read built page {}", src.display()));
        for token in tokens {
            assert!(
                html.contains(token),
                "BUG: built page {name}.html carries no {token}; rsbuild.config.ts and build.rs disagree"
            );
        }
        let html = stamp(&html, &build_rev, &build_time);
        gzip_to(html.as_bytes(), &out_dir.join(format!("{name}.html.gz")));
    }

    // The logo is a standalone `/logo.svg` asset (referenced by an
    // `<img>`, not inlined in the bundle), so it does not pass through
    // the bundler. Gzip it here too, so the handler can serve it with
    // `Content-Encoding: gzip` instead of the raw SVG.
    let logo = fs::read(manifest_dir.join("src/logo.svg")).expect("BUG: read src/logo.svg");
    gzip_to(&logo, &out_dir.join("logo.svg.gz"));

    // Rebuild when any frontend source or the build environment changes.
    for path in [
        "web",
        "dev",
        "src/logo.svg",
        "index.html",
        "login.html",
        "rsbuild.config.ts",
        "tsconfig.json",
        "globals.d.ts",
        "package.json",
        "package-lock.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=UBOS_WEBUI_DIST");
    println!("cargo:rerun-if-env-changed=UBOS_WEBUI_BUILD_REV");
    println!("cargo:rerun-if-env-changed=UBOS_WEBUI_BUILD_TIME");
}

/// Resolve the short commit hash for the build-info footer. The image
/// builds pass `UBOS_WEBUI_BUILD_REV` because the nix sandbox has no
/// `.git` directory. A dev shell sets nothing, so the build reads `git
/// rev-parse` against the crate's manifest directory when it runs, and
/// an unchanged tree keeps the stamp it has.
fn resolve_build_rev(manifest_dir: &Path) -> String {
    if let Ok(rev) = env::var("UBOS_WEBUI_BUILD_REV") {
        let trimmed = rev.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    git_short_rev(manifest_dir).unwrap_or_else(|| "unknown".to_string())
}

fn git_short_rev(manifest_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rev = String::from_utf8(output.stdout).ok()?;
    let trimmed = rev.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the build timestamp as a unix-epoch string. Prefers
/// `UBOS_WEBUI_BUILD_TIME`, which the image builds set to
/// `self.lastModified`; falls back to the system clock. A dev shell
/// leaves the variable unset on purpose: in a dirty tree the flake's
/// value is the moment of evaluation, and a value that changed on
/// every run rebuilt this crate and every firmware behind it on every
/// run. `SOURCE_DATE_EPOCH` is intentionally ignored: nix-shell pins
/// it to 1980-01-01, which would freeze every dev-shell page at that
/// date.
fn resolve_build_time() -> String {
    if let Ok(value) = env::var("UBOS_WEBUI_BUILD_TIME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("BUG: system clock before unix epoch")
        .as_secs()
        .to_string()
}
