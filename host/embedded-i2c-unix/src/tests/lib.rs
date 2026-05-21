use super::*;

#[test]
fn open_nonexistent_returns_error() {
    let result = I2c::open("/dev/i2c-does-not-exist");
    assert!(result.is_err());
}

#[test]
fn error_kind_maps_io_to_other() {
    use embedded_hal_async::i2c::Error as _;
    let error = Error::Io(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(matches!(
        error.kind(),
        embedded_hal_async::i2c::ErrorKind::Other
    ));
}
