//! Pre-compress the dashboard and login HTML at build time.
//!
//! `src/lib.rs` includes the gzipped outputs via `include_bytes!` and
//! serves them with `Content-Encoding: gzip`, so first paint stays inside
//! the TCP initcwnd budget without any runtime compression cost on the
//! MCU. Rebuilds when either source HTML changes.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;

/// Gzip-compress `source` and write it to `out_dir/<output_name>`.
fn compress_html(source: &[u8], out_dir: &Path, output_name: &str) {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    // `GzEncoder` writes into an in-memory `Vec`, so the only failure
    // mode is allocator OOM, which the build host treats as fatal.
    encoder
        .write_all(source)
        .expect("BUG: gzip encoder rejected HTML bytes");
    // `finish` flushes pending bytes; only allocator OOM can fail here.
    let compressed = encoder
        .finish()
        .expect("BUG: gzip encoder failed to finalize stream");
    // `OUT_DIR` is writable by definition during a build.
    fs::write(out_dir.join(output_name), compressed)
        .unwrap_or_else(|_| panic!("BUG: write {output_name} into OUT_DIR"));
}

fn main() {
    // Cargo always sets `OUT_DIR` for build scripts.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR unset by cargo"));

    let index = fs::read("src/index.html").expect("BUG: src/index.html missing from crate");
    compress_html(&index, &out_dir, "index.html.gz");

    let login = fs::read("src/login.html").expect("BUG: src/login.html missing from crate");
    compress_html(&login, &out_dir, "login.html.gz");

    println!("cargo:rerun-if-changed=src/index.html");
    println!("cargo:rerun-if-changed=src/login.html");
}
