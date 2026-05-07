//! Integration layer between `mining-protocol` adapters and the
//! `AsicSession` ASIC abstraction.
//!
//! Owns the `JobRing<2>`, `WorkRing<8>`, the work generator task,
//! and the visitor impl that drives both. Generic over
//! `MiningClient`, so V1 and V2 share the same integration code.
//! Replaces `ubos-backend`.

#![no_std]
