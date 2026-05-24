//! HTTP API surface for the firmware web UI and external clients.
//!
//! This crate is being grown incrementally. The initial slice contains
//! the cryptographic primitives (`csprng`), the credential store and
//! HTTP-Basic / session-cookie authentication paths (`auth`), and the
//! session table that backs the cookie path (`session`). Later commits
//! add CSRF tokens, the IP allow-list, REST schemas, the `/api/system`
//! dispatch, and the dashboard request handler.
#![no_std]

pub mod allow_list;
pub mod auth;
pub mod csprng;
pub mod csrf;
pub mod schema;
pub mod session;
pub mod system;

pub use csprng::seed_csprng;
