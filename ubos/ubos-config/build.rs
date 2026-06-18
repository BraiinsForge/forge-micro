//! Build script declaring the compile-time env vars that override the
//! factory-default pool endpoint, so cargo rebuilds this crate when they
//! change.
//!
//! [`crate::Config::default`] reads `POOL_USER`, `POOL_HOST`, and
//! `POOL_PORT` through `option_env!`. Cargo does not track `option_env!`
//! inputs on its own, so without these `rerun-if-env-changed` lines a
//! `POOL_USER=braiinstest cargo run` could silently reuse a stale build
//! that still has the previous (or empty) worker baked in.
fn main() {
    println!("cargo:rerun-if-env-changed=POOL_USER");
    println!("cargo:rerun-if-env-changed=POOL_HOST");
    println!("cargo:rerun-if-env-changed=POOL_PORT");
}
