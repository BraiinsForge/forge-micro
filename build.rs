//! Pre-compress the dashboard HTML at build time.
//!
//! `src/http.rs` includes the gzipped output via `include_bytes!` and
//! serves it with `Content-Encoding: gzip`, so first paint stays inside
//! the TCP initcwnd budget without any runtime compression cost on the
//! MCU. Rebuilds when the source HTML changes.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use flate2::write::GzEncoder;
use flate2::Compression;

fn main() {
    // The dashboard source ships with the crate; a missing file means
    // the working tree is broken, not a recoverable build failure.
    let source = fs::read("src/index.html").expect("BUG: src/index.html missing from crate");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    // `GzEncoder` writes into an in-memory `Vec`, so the only failure
    // mode is allocator OOM, which the build host treats as fatal.
    encoder
        .write_all(&source)
        .expect("BUG: gzip encoder rejected dashboard bytes");
    // Same in-memory write contract: `finish` flushes any pending
    // bytes into the backing `Vec`, so only allocator OOM can fail.
    let compressed = encoder
        .finish()
        .expect("BUG: gzip encoder failed to finalize stream");

    // Cargo always sets `OUT_DIR` for build scripts.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR unset by cargo"));
    // The output path lives under cargo's per-crate `OUT_DIR`, which
    // is writable by definition during a build.
    fs::write(out_dir.join("index.html.gz"), compressed)
        .expect("BUG: write index.html.gz into OUT_DIR");

    println!("cargo:rerun-if-changed=src/index.html");
}
