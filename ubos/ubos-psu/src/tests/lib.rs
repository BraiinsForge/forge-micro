use super::*;

#[test]
fn status_flags_default_all_clear() {
    let flags = StatusFlags::default();
    assert!(!flags.current_limit);
    assert!(!flags.thermal_shutdown);
    assert!(!flags.overvoltage);
    assert!(!flags.undervoltage);
}

#[test]
fn error_init_failed_carries_kind() {
    let err: Error<core::convert::Infallible> =
        Error::InitFailed(embedded_hal_async::i2c::ErrorKind::Bus);
    match err {
        Error::InitFailed(kind) => {
            assert!(matches!(kind, embedded_hal_async::i2c::ErrorKind::Bus));
        }
        _ => panic!("BUG: variant mismatch"),
    }
}
