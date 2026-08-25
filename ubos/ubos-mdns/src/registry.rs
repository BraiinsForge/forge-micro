// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Compile-time service-advertisement entries.
//!
//! Each crate that owns a network-visible port declares its entry next
//! to the code that owns the port via [`declare_service!`]. The macro
//! emits a plain `pub const ServiceEntry`, so the entry lives in flash
//! with no startup cost and no linker contract. Firmware composes its
//! own list of references at the [`run_mdns`](crate::run_mdns) call
//! site, which is the single place that decides which services the
//! board actually advertises.

/// One mDNS SRV + TXT advertisement.
///
/// `service` is the bare DNS-SD service label, e.g. `"_http"`; the
/// responder appends the transport (`._tcp`) and `.local.` suffix. `port`
/// is the transport port the responder will advertise in the SRV record.
/// `txt_keys` and `txt_values` are parallel slices of equal length; the
/// responder pairs them into the TXT record as `key=value`. Static
/// strings that do not vary per firmware (path, api variant) belong here;
/// per-board identity (`board`, `mcu`) is appended at runtime by the
/// responder.
pub struct ServiceEntry {
    /// DNS-SD service label without transport or `.local.` suffix,
    /// e.g. `"_http"`.
    pub service: &'static str,
    /// Port number advertised in the SRV record.
    pub port: u16,
    /// TXT record keys, paired one-to-one with `txt_values`.
    pub txt_keys: &'static [&'static str],
    /// TXT record values, paired one-to-one with `txt_keys`.
    pub txt_values: &'static [&'static str],
}

/// Declare an mDNS service entry from the crate that owns the port.
///
/// Expands to a plain `const ServiceEntry`. Firmware references the
/// resulting constant when it builds the `&[&ServiceEntry]` slice it
/// passes to [`run_mdns`](crate::run_mdns).
///
/// # Example
///
/// ```rust,ignore
/// use ubos_http::consts::Http;
/// ubos_mdns::declare_service!(pub HTTP_API_SERVICE = ubos_mdns::ServiceEntry {
///     service: "_http",
///     port: Http::PORT,
///     txt_keys: &["path"],
///     txt_values: &["/"],
/// });
/// ```
#[macro_export]
macro_rules! declare_service {
    ($vis:vis $name:ident = $value:expr) => {
        $vis const $name: $crate::ServiceEntry = $value;
    };
}
