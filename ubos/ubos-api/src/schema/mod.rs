//! Schema types for the `/api/system/*` REST surface.
//!
//! Each sub-module defines exactly one response struct. All structs
//! derive `serde::Serialize` and are serialized with
//! `serde-json-core` so they compile in `no_std` environments without
//! heap allocation.

pub mod generic_response;
pub mod system_asic;
pub mod system_info;
pub mod system_scoreboard;
pub mod system_statistics;

pub use generic_response::GenericResponse;
pub use system_asic::SystemAsic;
pub use system_info::SystemInfo;
pub use system_scoreboard::SystemScoreboardEntry;
pub use system_statistics::SystemStatistics;

/// Schema version discriminator published on every versioned response.
///
/// Machine clients dispatch on this value to confirm they are speaking
/// to a ubos device rather than a real reference-device.
pub const API_VARIANT: &str = "ubos-0.1";
