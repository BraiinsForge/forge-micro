//! Compose, minify, and pre-compress the dashboard and login pages.
//!
//! Each page is composed from a small set of source files:
//!   src/<name>.html — template with `<!--CSS-->` and `<!--JS-->` markers
//!   src/<name>.css  — page-specific styles
//!   src/<name>.js   — page-specific script
//!   src/common.css  — styles shared by every page, prepended to each
//!
//! For each page, this script concatenates `common.css` with the page
//! CSS, runs `esbuild --minify` on the combined CSS and the page JS,
//! splices the minified results into the HTML template, and gzips the
//! result. `src/lib.rs` includes the gzipped output via
//! `include_bytes!` and serves it with `Content-Encoding: gzip`, so
//! first paint stays inside the TCP initcwnd budget without any
//! runtime compression cost on the MCU.
//!
//! `esbuild` must be on PATH at build time. The Nix `arm` and `esp`
//! devshells provide it, and every firmware/ubos-webui flake check
//! that runs this build script lists it in `nativeBuildInputs`.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use flate2::write::GzEncoder;
use flate2::Compression;

const CSS_MARKER: &str = "<!--CSS-->";
const JS_MARKER: &str = "<!--JS-->";

/// Strip HTML comments, trim per-line indentation, and drop the
/// inter-line newlines. esbuild has no HTML loader; do the cheap
/// things in Rust so the gzipped payload does not ship the source's
/// indent and developer-only comments. Safe because every source
/// line begins with `<` after trim, so collapsing newlines never
/// fuses two text nodes into one word. CSS and JS arrive already
/// minified by esbuild.
fn minify_html(html: &str) -> String {
    let without_comments = strip_html_comments(html);
    let mut out = String::with_capacity(without_comments.len());
    for line in without_comments.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Plain `assert!` rather than `debug_assert!` so the contract
        // holds in cargo's release build-script invocation where debug
        // assertions are disabled.
        assert!(
            trimmed.starts_with('<'),
            "minify_html collapses newlines and would fuse two text nodes \
             into one word if a non-tag line slipped in; got: {trimmed:?}",
        );
        out.push_str(trimmed);
    }
    out
}

fn strip_html_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut remaining = html;
    loop {
        let Some(start) = remaining.find("<!--") else {
            out.push_str(remaining);
            return out;
        };
        out.push_str(&remaining[..start]);
        let after = &remaining[start + 4..];
        let Some(end) = after.find("-->") else {
            return out;
        };
        remaining = &after[end + 3..];
    }
}

/// Run `esbuild --minify` on `source` with the given loader and return
/// the minified output as a string. esbuild reads stdin and writes
/// stdout when no input path is supplied; this avoids touching the
/// filesystem for intermediate artifacts.
fn esbuild_minify(source: &str, loader: &str) -> String {
    // esbuild is on PATH because every devshell and flake check that
    // invokes this build script lists `pkgs.esbuild` explicitly.
    let mut child = Command::new("esbuild")
        .arg("--minify")
        .arg(format!("--loader={loader}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect(
            "BUG: esbuild missing from PATH; enter one of the nix devshells \
             (`arm`, `esp`, `host`, `bmm`) first, or add `pkgs.esbuild` to the flake \
             derivation's `nativeBuildInputs`",
        );
    // `stdin` is `Some` because the spawn() above asked for a piped
    // stdin, and write_all succeeds because esbuild drains its stdin
    // before exiting.
    child
        .stdin
        .as_mut()
        .expect("BUG: esbuild stdin pipe missing")
        .write_all(source.as_bytes())
        .expect("BUG: write to esbuild stdin");
    // `child` was spawned above and is still owned here; wait_with_output
    // collects exit status plus stdout/stderr after esbuild closes them.
    let output = child
        .wait_with_output()
        .expect("BUG: esbuild process wait failed");
    // Forward esbuild warnings to the build script's stderr so they
    // appear in `cargo build` output even on a successful exit.
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(
        output.status.success(),
        "esbuild exited {:?}",
        output.status,
    );
    // esbuild emits UTF-8 minified output regardless of the input loader.
    String::from_utf8(output.stdout).expect("BUG: esbuild stdout not utf8")
}

/// Read template + page assets, splice in minified `<style>` and
/// `<script>` bodies, gzip, and write the result to
/// `OUT_DIR/<name>.html.gz`.
fn build_page(common_css: &str, name: &str, out_dir: &Path) {
    let template_path = format!("src/{name}.html");
    let css_path = format!("src/{name}.css");
    let js_path = format!("src/{name}.js");

    let template =
        fs::read_to_string(&template_path).unwrap_or_else(|_| panic!("BUG: read {template_path}"));
    let page_css = fs::read_to_string(&css_path).unwrap_or_else(|_| panic!("BUG: read {css_path}"));
    let page_js = fs::read_to_string(&js_path).unwrap_or_else(|_| panic!("BUG: read {js_path}"));

    let combined_css = format!("{common_css}\n{page_css}");
    let minified_css = esbuild_minify(&combined_css, "css");
    let minified_js = esbuild_minify(&page_js, "js");

    assert!(
        template.contains(CSS_MARKER),
        "{template_path}: missing {CSS_MARKER}"
    );
    assert!(
        template.contains(JS_MARKER),
        "{template_path}: missing {JS_MARKER}"
    );

    let inlined = template
        .replace(CSS_MARKER, &format!("<style>{minified_css}</style>"))
        .replace(JS_MARKER, &format!("<script>{minified_js}</script>"));
    let html = minify_html(&inlined);

    // GzEncoder over a Vec never returns Err: the backing buffer grows
    // on demand for the write and the deflate trailer for finish.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(html.as_bytes())
        .expect("BUG: gzip encoder rejected HTML bytes");
    let compressed = encoder
        .finish()
        .expect("BUG: gzip encoder failed to finalize stream");

    fs::write(out_dir.join(format!("{name}.html.gz")), compressed)
        .unwrap_or_else(|_| panic!("BUG: write {name}.html.gz into OUT_DIR"));

    println!("cargo:rerun-if-changed={template_path}");
    println!("cargo:rerun-if-changed={css_path}");
    println!("cargo:rerun-if-changed={js_path}");
}

fn main() {
    // OUT_DIR is set by cargo for every build script invocation.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR unset by cargo"));

    // src/common.css is checked into the crate sources.
    let common_css = fs::read_to_string("src/common.css").expect("BUG: read src/common.css");

    build_page(&common_css, "index", &out_dir);
    build_page(&common_css, "login", &out_dir);

    println!("cargo:rerun-if-changed=src/common.css");
}
