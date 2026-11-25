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
//! `rsbuild build` emits one self-contained, minified HTML file per
//! entry. This script stamps those files and gzips each into
//! `OUT_DIR/<name>.html.gz` for `src/lib.rs` to embed, so first paint
//! costs no runtime compression on the MCU.
//!
//! Gzip rather than Brotli: a browser advertises `br` only over HTTPS
//! and on `localhost`, and the board is reached over plain HTTP by IP,
//! so a Brotli response would never be requested. Zopfli emits a
//! standard gzip stream a few percent smaller than flate2 at level 9,
//! and build time does not matter here where embedded bytes do.
//!
//! Two ways to obtain the built pages:
//!   * `UBOS_WEBUI_DIST` names a prebuilt dist. The flake checks and the
//!     dev shells set it to a hermetic `buildNpmPackage` derivation
//!     output; the header of `nix/shells.nix` says why the shells do.
//!   * Otherwise `npm run build` runs in the crate directory, which is
//!     the path for a build outside every shell that sets the variable.
//!
//! The brand is stamped here rather than built into the bundle:
//! `rsbuild.config.ts` emits one token per value and this script replaces
//! them. So one built dist is cached across every board and serves every
//! brand, while the pages a firmware embeds carry the brand of the board
//! it was built for. `brand/README.md` documents the brand file and the
//! rules a new brand must obey.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use std::num::NonZeroU64;

/// Matches the upstream binary's maximum-compression setting. Gains
/// flatten well before it and the inputs are small, so the cost is
/// seconds of build time.
const ZOPFLI_ITERATIONS: u64 = 1000;

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

/// The tokens `rsbuild.config.ts` emits for the brand facts, replaced by
/// [`stamp`] in each built page. The page title and the wordmark `alt`
/// text take the name, and a `<style>` block in the head takes the
/// three colours.
const NAME_TOKEN: &str = "@UBOS_BRAND_NAME@";
const ACCENT_TOKEN: &str = "@UBOS_BRAND_ACCENT@";
const ACCENT_HI_TOKEN: &str = "@UBOS_BRAND_ACCENT_HI@";
const ACCENT_TEXT_TOKEN: &str = "@UBOS_BRAND_ACCENT_TEXT@";
const ICON_TOKEN: &str = "@UBOS_BRAND_ICON@";

/// The built pages, each beside the tokens it must carry. A page missing
/// a token it names was built from a config that no longer matches this
/// script, which is a bug rather than a page to ship unstamped.
const PAGES: [(&str, &[&str]); 2] = [
    (
        "index",
        &[
            NAME_TOKEN,
            ACCENT_TOKEN,
            ACCENT_HI_TOKEN,
            ACCENT_TEXT_TOKEN,
            ICON_TOKEN,
        ],
    ),
    (
        "login",
        &[
            NAME_TOKEN,
            ACCENT_TOKEN,
            ACCENT_HI_TOKEN,
            ACCENT_TEXT_TOKEN,
            ICON_TOKEN,
        ],
    ),
];

/// The brand facts a page carries, read from `$UBOS_BRAND/brand.json`.
struct Brand {
    name: String,
    accent: String,
    accent_hi: String,
    accent_text: String,
}

/// Read the declared brand. Every key is required: a default here would
/// be a second copy of every value, and the first edit would move one
/// copy only.
fn read_brand(dir: &Path) -> Brand {
    let path = dir.join("brand.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("BUG: read brand file {}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("BUG: {} is not JSON: {error}", path.display()));
    let string = |key: &str| {
        json.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("BUG: {} has no string key `{key}`", path.display()))
            .to_string()
    };
    Brand {
        name: string("name"),
        accent: string("accent"),
        accent_hi: string("accentHi"),
        accent_text: string("accentText"),
    }
}

/// Replace the brand tokens in a built page. A page that carries none of
/// them comes back unchanged.
fn stamp(html: &str, brand: &Brand, icon: &str) -> String {
    html.replace(NAME_TOKEN, &brand.name)
        .replace(ACCENT_TOKEN, &brand.accent)
        .replace(ACCENT_HI_TOKEN, &brand.accent_hi)
        .replace(ACCENT_TEXT_TOKEN, &brand.accent_text)
        .replace(ICON_TOKEN, icon)
}

/// The brand's tab icon as a `data:` URI.
///
/// A board answers as many requests at once as its HTTP pool is deep,
/// the smallest pool is two, and an icon linked by path costs a third
/// request on every page load. A browser offered no icon asks for
/// `/favicon.ico` by itself, so only a `data:` URI costs nothing.
fn icon_data_uri(dir: &Path) -> String {
    use base64::Engine as _;

    let path = dir.join("icon.png");
    let bytes = fs::read(&path).unwrap_or_else(|_| panic!("BUG: read {}", path.display()));
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:image/png;base64,{encoded}")
}

/// Run `npm run build` in the crate directory. Installs dependencies
/// first when `node_modules` is absent so a fresh checkout builds
/// without a manual `npm ci`.
fn run_web_build(manifest_dir: &Path) {
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
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR unset by cargo"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("BUG: CARGO_MANIFEST_DIR unset"));

    // `relative = true` on the declaration resolves the value against
    // the board's crate directory, so this is an absolute path whichever
    // directory cargo ran in.
    let brand_dir = PathBuf::from(env::var_os("UBOS_BRAND").expect(
        "BUG: UBOS_BRAND is not set. Declare the brand directory in the board's .cargo/config.toml",
    ));
    let brand = read_brand(&brand_dir);
    let icon = icon_data_uri(&brand_dir);

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
        let html = stamp(&html, &brand, &icon);
        gzip_to(html.as_bytes(), &out_dir.join(format!("{name}.html.gz")));
    }

    // The page tests assert that each page carries the resolved brand
    // name, which is what proves the tokens were replaced with the brand
    // rather than merely removed.
    fs::write(out_dir.join("brand-name"), &brand.name).expect("BUG: write brand name");

    // The wordmark is referenced by an `<img>` and never passes through
    // the bundler, so it is gzipped here for the handler to serve with
    // `Content-Encoding: gzip`.
    let logo_path = brand_dir.join("logo.svg");
    let logo = fs::read(&logo_path).unwrap_or_else(|_| panic!("BUG: read {}", logo_path.display()));
    gzip_to(&logo, &out_dir.join("logo.svg.gz"));

    // Rebuild when any frontend source or the build environment changes.
    for path in [
        "web",
        "dev",
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

    // The brand is three files outside the crate, so each one needs its
    // own trigger. A trigger naming a file that does not exist makes
    // cargo run this script on every build, and each run costs a Zopfli
    // pass, so these paths are the ones read above and nothing else.
    println!("cargo:rerun-if-env-changed=UBOS_BRAND");
    println!(
        "cargo:rerun-if-changed={}",
        brand_dir.join("brand.json").display()
    );
    println!("cargo:rerun-if-changed={}", logo_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        brand_dir.join("icon.png").display()
    );
}
