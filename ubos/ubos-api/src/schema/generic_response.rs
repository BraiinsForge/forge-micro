//! Generic single-message response body used by 501 stubs and, in a
//! later phase, by action-endpoint acknowledgements.

use serde::Serialize;

/// A response body carrying a single human-readable message string.
///
/// Serialized as `{"message":"…"}`. Used wherever the firmware cannot
/// or does not perform the requested operation and needs to explain
/// why in a machine-readable envelope.
#[derive(Serialize)]
pub struct GenericResponse<'response> {
    /// Short description of the outcome.
    pub message: &'response str,
}
