//! Tests for `SessionError::is_rejection`'s permanent/transient split.

use super::*;

use mining_protocol::SessionRejection;

#[test]
fn pool_refusals_are_rejections() {
    for cause in [
        SessionRejection::SubscribeRefused,
        SessionRejection::AuthorizeRefused,
    ] {
        let error = SessionError::<()>::Session(Error::SessionRejected(cause));
        assert!(error.is_rejection());
    }
    assert!(SessionError::<()>::Session(Error::AuthFailed).is_rejection());
}

#[test]
fn transient_failures_are_not_rejections() {
    assert!(!SessionError::<()>::Connect(()).is_rejection());
    for error in [Error::Transport, Error::Decode, Error::Adapter] {
        assert!(!SessionError::<()>::Session(error).is_rejection());
    }
}
