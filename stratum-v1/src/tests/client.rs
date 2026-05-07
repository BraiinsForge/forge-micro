//! Integration tests for `StratumV1Client` connect+drive paths
//! using an in-memory tokio duplex as the transport.

extern crate std;

use std::vec::Vec;

use super::*;

use crate::Sv1Config;
use embedded_io_adapters::tokio_1::FromTokio;
use mining_protocol::{Error, MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, SubmitResult};
use tokio::io::AsyncReadExt;

const TEST_CONFIG: Sv1Config = Sv1Config {
    user: "alice.worker1",
    password: "x",
};

/// Spy visitor used by tests to count callbacks. The bodies just
/// record so tests can assert on counts.
#[derive(Default)]
struct CountingVisitor {
    new_jobs: usize,
    submit_results: usize,
    session_updates: usize,
}

impl MiningClientVisitor for CountingVisitor {
    fn visit_new_job(&mut self, _job: MiningJob<'_>) {
        self.new_jobs += 1;
    }
    fn visit_submit_result(&mut self, _result: SubmitResult<'_>) {
        self.submit_results += 1;
    }
    fn visit_session_update(&mut self, _update: SessionUpdate<'_>) {
        self.session_updates += 1;
    }
}

#[tokio::test]
async fn connect_pipelines_handshake_messages() {
    let (a, b) = tokio::io::duplex(4096);
    let mut a_io = FromTokio::new(a);
    let mut b_io = b;

    let mut client = StratumV1Client::new(&TEST_CONFIG);
    client
        .connect(&mut a_io)
        .await
        .expect("connect must succeed against a writable pipe");

    // Read the bytes the pool side received.
    let mut received = [0u8; 4096];
    let n = b_io.read(&mut received).await.expect("read");
    let bytes = &received[..n];

    // Expect three newline-terminated JSON-RPC requests, in order.
    let request_lines: Vec<&[u8]> = bytes
        .split(|byte: &u8| *byte == b'\n')
        .filter(|chunk: &&[u8]| !chunk.is_empty())
        .collect();
    assert_eq!(
        request_lines.len(),
        3,
        "expected configure, subscribe, authorize"
    );

    let configure = core::str::from_utf8(request_lines[0]).expect("utf-8");
    assert!(configure.contains("\"method\":\"mining.configure\""), "{configure}");
    assert!(configure.contains("\"id\":1"), "{configure}");
    assert!(
        configure.contains("\"version-rolling\""),
        "configure must request version-rolling: {configure}"
    );

    let subscribe = core::str::from_utf8(request_lines[1]).expect("utf-8");
    assert!(subscribe.contains("\"method\":\"mining.subscribe\""), "{subscribe}");
    assert!(subscribe.contains("\"id\":2"), "{subscribe}");

    let authorize = core::str::from_utf8(request_lines[2]).expect("utf-8");
    assert!(authorize.contains("\"method\":\"mining.authorize\""), "{authorize}");
    assert!(authorize.contains("\"id\":3"), "{authorize}");
    assert!(
        authorize.contains("alice.worker1"),
        "authorize must carry the configured user: {authorize}"
    );
}

#[tokio::test]
async fn drive_returns_when_a_complete_line_lands() {
    let (a, mut b) = tokio::io::duplex(4096);
    let mut a_io = FromTokio::new(a);
    let mut client = StratumV1Client::new(&TEST_CONFIG);

    // Hand the pool side a fake configure response. drive() is what
    // the firmware loop calls; it should resolve as soon as one
    // complete line is buffered.
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = b.write_all(br#"{"id":1,"result":{"version-rolling":true,"version-rolling.mask":"1fffe000"},"error":null}"#).await;
        let _ = b.write_all(b"\n").await;
    });

    client.drive(&mut a_io).await.expect("drive must surface the buffered line");
    // accept() drains the pending line so subsequent drives don't
    // re-see it. Stub for now; richer assertions land with the
    // dispatch implementation.
    let mut visitor = CountingVisitor::default();
    client.accept(&mut visitor);
}

#[tokio::test]
async fn drive_propagates_transport_close_as_error() {
    let (a, b) = tokio::io::duplex(4096);
    let mut a_io = FromTokio::new(a);
    drop(b); // close the pool side immediately

    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let err = client.drive(&mut a_io).await.expect_err("closed pipe must error");
    assert!(matches!(err, Error::Transport));
}
