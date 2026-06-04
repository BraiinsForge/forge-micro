//! Shared HTTP middleware: allow-list, authentication, sessions, CSRF, CSPRNG.
//!
//! Consumed by every listener crate (`ubos-webui` on port 80, `ubos-api`
//! on port 8080) so the credential store, session table, and CSRF
//! tokens are global to the firmware rather than per-port.

#![no_std]

pub mod allow_list;
pub mod auth;
pub mod csprng;
pub mod csrf;
pub mod session;

#[cfg(test)]
mod test_time_stubs {
    // `session` and `csrf` call `embassy_time::Instant::now()`.
    // `embassy-time-driver` declares the hook as `extern "Rust"`; no
    // hardware driver is linked into the host test binary, so satisfy
    // the linker with a stub that returns a fixed tick of zero.
    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_now() -> u64 {
        0
    }

    #[allow(clippy::no_mangle_with_rust_abi)]
    #[unsafe(no_mangle)]
    fn _embassy_time_schedule_wake(_at: u64, _waker: &core::task::Waker) {
        // No scheduler in the host binary; test paths do not block on
        // timers so leaving this empty is safe.
    }
}
