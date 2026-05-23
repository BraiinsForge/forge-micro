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
/// use ubos_core::consts::{Http, Mdns as MdnsConsts};
/// ubos_mdns::declare_service!(pub HTTP_API_SERVICE = ubos_mdns::ServiceEntry {
///     service: MdnsConsts::HTTP_SERVICE,
///     port: Http::PORT,
///     txt_keys: &["path"],
///     txt_values: &[MdnsConsts::HTTP_PATH],
/// });
/// ```
#[macro_export]
macro_rules! declare_service {
    ($vis:vis $name:ident = $value:expr) => {
        $vis const $name: $crate::ServiceEntry = $value;
    };
}
