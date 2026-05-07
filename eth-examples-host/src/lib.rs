//! Host-side integration harness for `stratum-client`.
//!
//! Drives the same Embassy stack that runs on the firmwares against a
//! Linux TUN/TAP interface, so V1 and V2 mining sessions can be
//! exercised end-to-end against a real pool without flashing a board.
//! Host-only (uses std); not built into firmware images.
