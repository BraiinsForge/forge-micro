// Copyright (C) 2026  Braiins Systems s.r.o.
//
// All rights reserved. This file is proprietary and may not be copied,
// distributed, or disclosed except as authorized in writing.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Put `memory.x` in our output directory and ensure it's
    // on the linker search path.
    let out = &PathBuf::from(env::var_os("OUT_DIR").expect("BUG: OUT_DIR not set during build"));
    File::create(out.join("memory.x"))
        .expect("BUG: failed to create memory.x in OUT_DIR")
        .write_all(include_bytes!("memory.x"))
        .expect("BUG: failed to write memory.x");
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    // link-rp.x (from embassy-rp) places the `.boot2` section at
    // ORIGIN(BOOT2). The bootloader uses this so the RP2040 bootrom can
    // find and verify the second-stage loader before handing off control.
    println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    if env::var("CARGO_FEATURE_DEFMT").is_ok() {
        println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
    }
}
