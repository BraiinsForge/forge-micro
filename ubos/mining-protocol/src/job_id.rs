//! Generic, protocol-agnostic job identifier.
//!
//! Adapters assign a fresh `JobId` for each new job they surface to
//! the visitor and keep an internal map from `JobId` to the wire-form
//! id (V1 string, V2 u32). Consumers downstream of the trait
//! boundary (`stratum-client`, solvers) only ever see `JobId` and
//! never touch either wire form.

/// 32-bit opaque token correlating a stratum job, its outstanding
/// work assignments, and the eventual share submission.
///
/// `Copy` is intentional: this is a 4-byte handle, not an owning
/// reference.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct JobId(pub u32);
