//! Integration tests for `StratumV1Client` connect+drive paths
//! using an in-memory tokio duplex as the transport.

extern crate std;

use std::vec::Vec;

use super::*;

use crate::Sv1Config;
use embedded_io_adapters::tokio_1::FromTokio;
use mining_protocol::{
    Error, JobId, MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, Share, SubmitResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TEST_CONFIG: Sv1Config = Sv1Config {
    user: "alice.worker1",
    password: "x",
};

/// Spy visitor that records observable parameters so tests can
/// assert on what dispatch produced.
#[derive(Default)]
struct RecordingVisitor {
    new_jobs: u32,
    submit_results: u32,
    session_updates: u32,
    last_target: Option<[u8; 32]>,
    last_version_mask: Option<u32>,
    last_extranonce_2_size: Option<u8>,
    last_clean_jobs: Option<bool>,
    last_job_version: Option<u32>,
}

impl MiningClientVisitor for RecordingVisitor {
    fn visit_new_job(&mut self, job: MiningJob<'_>) {
        self.new_jobs += 1;
        self.last_clean_jobs = Some(job.clean_jobs);
        self.last_job_version = Some(job.version);
        self.last_extranonce_2_size = Some(job.extranonce_2_size);
    }
    fn visit_submit_result(&mut self, _result: SubmitResult<'_>) {
        self.submit_results += 1;
    }
    fn visit_session_update(&mut self, update: SessionUpdate<'_>) {
        self.session_updates += 1;
        match update {
            SessionUpdate::SetTarget(target) => self.last_target = Some(*target),
            SessionUpdate::SetVersionMask(mask) => self.last_version_mask = Some(mask),
            SessionUpdate::SetExtranonce { en2_size, .. } => {
                self.last_extranonce_2_size = Some(en2_size);
            }
            SessionUpdate::Reconnect { .. } => {}
        }
    }
}

/// Drive once against `b`'s payload, then dispatch into `visitor`.
async fn pump_one(client: &mut StratumV1Client, payload: &[u8]) -> RecordingVisitor {
    let (a, mut b) = tokio::io::duplex(8192);
    let mut a_io = FromTokio::new(a);
    let payload_owned: Vec<u8> = payload.to_vec();
    tokio::spawn(async move {
        let _ = b.write_all(&payload_owned).await;
        let _ = b.write_all(b"\n").await;
    });
    client.drive(&mut a_io).await.expect("drive ok");
    let mut visitor = RecordingVisitor::default();
    client.accept(&mut visitor);
    visitor
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
async fn drive_propagates_transport_close_as_error() {
    let (a, b) = tokio::io::duplex(4096);
    let mut a_io = FromTokio::new(a);
    drop(b);

    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let err = client.drive(&mut a_io).await.expect_err("closed pipe must error");
    assert!(matches!(err, Error::Transport));
}

#[tokio::test]
async fn accept_dispatches_set_difficulty_as_set_target() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let visitor = pump_one(
        &mut client,
        br#"{"id":null,"method":"mining.set_difficulty","params":[1024]}"#,
    )
    .await;

    assert_eq!(visitor.session_updates, 1);
    let target = visitor.last_target.expect("target recorded");
    // diff > 0 ⇒ target is non-trivial (not all-zero, not all-FF).
    assert!(target.iter().any(|byte| *byte != 0));
    assert!(target.iter().any(|byte| *byte != 0xFF));
}

#[tokio::test]
async fn accept_dispatches_set_version_mask() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let visitor = pump_one(
        &mut client,
        br#"{"id":null,"method":"mining.set_version_mask","params":["1fffe000"]}"#,
    )
    .await;
    assert_eq!(visitor.session_updates, 1);
    assert_eq!(visitor.last_version_mask, Some(0x1fff_e000));
}

#[tokio::test]
async fn accept_dispatches_subscribe_response_as_set_extranonce() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let visitor = pump_one(
        &mut client,
        br#"{"id":2,"result":[[["mining.notify","abc"]],"deadbeef",4],"error":null}"#,
    )
    .await;
    assert_eq!(visitor.session_updates, 1);
    assert_eq!(visitor.last_extranonce_2_size, Some(4));
}

#[tokio::test]
async fn accept_dispatches_notify_as_new_job() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);

    // Prime extranonce_1 / extranonce_2_size so the notify can build
    // a complete MiningJob.
    let _ = pump_one(
        &mut client,
        br#"{"id":2,"result":[[["mining.notify","abc"]],"deadbeef",4],"error":null}"#,
    )
    .await;

    let notify = build_notify_line(true);
    let visitor = pump_one(&mut client, notify.as_bytes()).await;
    assert_eq!(visitor.new_jobs, 1);
    assert_eq!(visitor.last_clean_jobs, Some(true));
    assert_eq!(visitor.last_job_version, Some(0x2000_0004));
    assert_eq!(visitor.last_extranonce_2_size, Some(4));
}

#[tokio::test]
async fn submit_rejects_unknown_job_id() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);
    let share = Share {
        id: JobId(999),
        nonce: 0,
        ntime: 0,
        version_bits: 0,
        extranonce_2: &[],
    };
    assert!(matches!(
        client.submit(&share),
        Err(Error::UnknownJobId),
    ));
}

#[tokio::test]
async fn submit_encodes_mining_submit_with_wire_job_id() {
    let mut client = StratumV1Client::new(&TEST_CONFIG);

    // Prime extranonce_1 / extranonce_2_size and a JobId mapping by
    // pumping a subscribe response and a notify.
    let _ = pump_one(
        &mut client,
        br#"{"id":2,"result":[[["mining.notify","abc"]],"deadbeef",4],"error":null}"#,
    )
    .await;
    let visitor = pump_one(&mut client, build_notify_line(false).as_bytes()).await;
    assert_eq!(visitor.new_jobs, 1);

    // The first job allocated by the adapter is JobId(0).
    let share = Share {
        id: JobId(0),
        nonce: 0xdead_beef,
        ntime: 0x6044_9522,
        version_bits: 0,
        extranonce_2: &[0xaa, 0xbb, 0xcc, 0xdd],
    };
    client.submit(&share).expect("submit must accept the known JobId");

    // Drain the queued submit by driving once. drive() flushes the
    // outbound slot before its read loop, so we pre-stage a single
    // newline on the pool side that satisfies the read immediately.
    let (a, mut b) = tokio::io::duplex(4096);
    let mut a_io = FromTokio::new(a);
    b.write_all(b"\n").await.expect("pre-stage newline");
    let _ = client.drive(&mut a_io).await;

    let mut received = [0u8; 4096];
    let n = b.read(&mut received).await.expect("read");
    let bytes = core::str::from_utf8(&received[..n]).expect("utf-8");

    assert!(bytes.contains("\"method\":\"mining.submit\""), "{bytes}");
    assert!(bytes.contains("\"1d2e0c4d3d\""), "wire job_id must round-trip: {bytes}");
    assert!(bytes.contains("\"aabbccdd\""), "extranonce_2 hex: {bytes}");
    assert!(bytes.contains("\"deadbeef\""), "nonce hex: {bytes}");
}

/// Real-pool notify shape with a 12-branch merkle path. Borrowed
/// from the parser test fixtures.
fn build_notify_line(clean_jobs: bool) -> std::string::String {
    let merkle = "[\"ae23055e00f0f697cc3640124812d96d4fe8bdfa03484c1c638ce5a1c0e9aa81\",\
        \"980fb87cb61021dd7afd314fcb0dabd096f3d56a7377f6f320684652e7410a21\",\
        \"a52e9868343c55ce405be8971ff340f562ae9ab6353f07140d01666180e19b52\",\
        \"7435bdfa004e603953b2ed39f118803934d9cf17b06d979ceb682f2251bafac2\",\
        \"2a91f061a22d27cb8f44eea79938fb241ebeb359891aa907f05ffde7ed44e52e\",\
        \"302401f80eb5e958155135e25200bb8ea181ad2d05e804a531c7314d86403cdc\",\
        \"318ecb6161eb9b4cfd802bd730e2d36c167ddf102e70aa7b4158e2870dd47392\",\
        \"1114332a9858e0cf84b2425bb1e59eaabf91dd102d114aa443d57fc1b3beb0c9\",\
        \"f43f38095c810613ed795a44d9fab02ff25269706f454885db9be05cdf9c06e1\",\
        \"3e2fc26b27fddc39668b59099cd9635761bb72ed92404204e12bdff08b16fb75\",\
        \"463c19427286342120039a83218fa87ce45448e246895abac11fff0036076758\",\
        \"03d287f655813e540ddb9c4e7aeb922478662b0f5d8e9d0cbd564b20146bab76\"]";
    let coinbase_1 = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4b03a5020cfabe6d6d379ae882651f6469f2ed6b8b40a4f9a4b41fd838a3ad6de8cba775f4e8f1d3080100000000000000";
    let coinbase_2 = "41903d4c1b2f736c7573682f0000000003ca890d27000000001976a9147c154ed1dc59609e3d26abb2df2ea3d587cd8c4188ac00000000000000002c6a4c2952534b424c4f434b3a4cb4cb2ddfc37c41baf5ef6b6b4899e3253a8f1dfc7e5dd68a5b5b27005014ef0000000000000000266a24aa21a9ed5caa249f1af9fbf71c986fea8e076ca34ae3514fb2f86400561b28c7b15949bf00000000";
    let prev_hash = "ef4b9a48c7986466de4adc002f7337a6e121bc43000376ea0000000000000000";
    let clean = if clean_jobs { "true" } else { "false" };
    std::format!(
        r#"{{"id":null,"method":"mining.notify","params":["1d2e0c4d3d","{prev_hash}","{coinbase_1}","{coinbase_2}",{merkle},"20000004","1705c739","64495522",{clean}]}}"#
    )
}
