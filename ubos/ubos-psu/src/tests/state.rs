use super::*;
use crate::{BmmPsu, BmmPsuDescriptor};
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

#[test]
fn ensure_initialized_runs_full_init_then_marks_initialized() {
    let config = BmmPsuDescriptor::DEFAULT;
    let bus = Mock::new(&test_support::init_transactions(&config));
    let mut state = State::new(bus, config);
    run(BmmPsu::ensure_initialized(&mut state)).expect("BUG: init failed");
    assert_eq!(state.init, InitStatus::Initialized);
    state.i2c.done();
}

#[test]
fn ensure_initialized_caches_success() {
    let config = BmmPsuDescriptor::DEFAULT;
    let bus = Mock::new(&test_support::init_transactions(&config));
    let mut state = State::new(bus, config);
    run(BmmPsu::ensure_initialized(&mut state)).expect("BUG: first init failed");
    // Second call must not touch the bus.
    run(BmmPsu::ensure_initialized(&mut state)).expect("BUG: cached init returned error");
    state.i2c.done();
}

#[test]
fn ensure_initialized_caches_failure_kind() {
    let config = BmmPsuDescriptor::DEFAULT;
    // First transaction errors out: NACK on the watchdog write.
    let expectations = [
        Transaction::write(config.watchdog_address, vec![0x7a, 0x00]).with_error(
            embedded_hal_async::i2c::ErrorKind::NoAcknowledge(
                embedded_hal_async::i2c::NoAcknowledgeSource::Address,
            ),
        ),
    ];
    let bus = Mock::new(&expectations);
    let mut state = State::new(bus, config);
    let first = run(BmmPsu::ensure_initialized(&mut state));
    assert!(matches!(first, Err(Error::Bus(_))));
    match state.init {
        InitStatus::Failed(kind) => {
            assert!(matches!(kind, ErrorKind::NoAcknowledge(_)));
        }
        other => panic!("BUG: expected Failed, got {other:?}"),
    }
    let second = run(BmmPsu::ensure_initialized(&mut state));
    assert!(matches!(second, Err(Error::InitFailed(_))));
    state.i2c.done();
}
