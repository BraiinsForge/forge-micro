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

//! Tests of work publication, session teardown and outbound arbitration.

use super::*;
use embassy_time::Instant;

use crate::asic_session::AsicSession;
use crate::extension::{NoExtension, SessionExtension};
use crate::state::StratumState;
use mining_protocol::{ExtensionFrame, JobId, LazyMerkle, MiningClientVisitor, MiningJob};

const SAMPLE_PREV_HASH: [u8; 32] = [0x11; 32];
const SAMPLE_TARGET: [u8; 32] = [0x22; 32];
const SAMPLE_ROOT: [u8; 32] = [0x33; 32];

fn sample_job(id: u32) -> MiningJob<'static> {
    static PREV_HASH: [u8; 32] = SAMPLE_PREV_HASH;
    static TARGET: [u8; 32] = SAMPLE_TARGET;
    static ROOT: [u8; 32] = SAMPLE_ROOT;
    MiningJob {
        id: JobId(id),
        prev_hash: &PREV_HASH,
        target: &TARGET,
        version: 0x2000_0000,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
        clean_jobs: false,
        extranonce_2_size: 0,
        merkle: LazyMerkle::Final(&ROOT),
    }
}

/// Publish `n` times against a single fresh job with a fixed `step`
/// and collect the resulting `ntime_offset` after each publish.
fn offsets(step: u32, n: usize) -> heapless::Vec<u32, 16> {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    let mut out = heapless::Vec::new();
    for _ in 0..n {
        state.publish_work(asic.pool_handle(0), step);
        let offset = state.job_ring.current().expect("slot present").ntime_offset;
        out.push(offset).expect("fits capacity");
    }
    out
}

#[test]
fn rolls_one_second_per_publish() {
    // The ticker branch always passes step=1 regardless of the
    // configured period: the period spaces publishes, it does not
    // scale the roll, so ntime never outruns wall clock.
    assert_eq!(offsets(1, 4).as_slice(), &[1, 2, 3, 4]);
}

#[test]
fn advances_by_the_given_step() {
    // publish_work's math is generic over the step even though the
    // run loop only ever passes 0 (job-acceptance publish) or 1
    // (ticker publish).
    assert_eq!(offsets(3, 3).as_slice(), &[3, 6, 9]);
}

#[test]
fn offset_caps_at_ntime_roll_max_secs() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    // A period large enough that two ticks would overshoot the cap.
    let period = NTIME_ROLL_MAX_SECS / 2 + 100;
    state.publish_work(asic.pool_handle(0), period);
    state.publish_work(asic.pool_handle(0), period);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        NTIME_ROLL_MAX_SECS
    );
}

#[test]
fn zero_step_leaves_offset_unchanged() {
    // The job-acceptance publish (no ticker running) passes step=0:
    // ntime stays pinned to the job's base.
    assert_eq!(offsets(0, 5).as_slice(), &[0, 0, 0, 0, 0]);
}

#[test]
fn offset_resets_on_new_job() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();

    state.publish_work(asic.pool_handle(0), 1);
    state.publish_work(asic.pool_handle(0), 1);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        2
    );

    state.visit_new_job(sample_job(2));
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        0
    );

    state.publish_work(asic.pool_handle(0), 1);
    assert_eq!(
        state.job_ring.current().expect("slot present").ntime_offset,
        1
    );
}

/// The granted version-rolling mask rides every published
/// `WorkAssignment`: `StratumState::version_mask` is read straight
/// into the assignment `publish_work` builds, so a chip driver
/// downstream of the `AsicSession` sees which bits it may roll.
#[test]
fn version_mask_reaches_the_published_work_assignment() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    state.version_mask = 0x1FFF_E000;
    let asic: AsicSession = AsicSession::new();

    state.publish_work(asic.pool_handle(0), 0);

    let mut receiver = asic.work_receiver(0).expect("slot 0 has a receiver");
    let assignment = receiver
        .try_get()
        .expect("publish_work published an assignment");
    assert_eq!(assignment.version_mask, 0x1FFF_E000);
}

/// Mirrors the `last_published != Some(current_job_id)` guard in
/// `run_stratum_client_inner`'s `Either3::First` arm: with no ticker,
/// that comparison is the only thing deciding whether a given
/// `client.accept()` triggers a `publish_work` call. `JobId`'s
/// `Copy + PartialEq` derive is what makes the comparison viable
/// without borrowing the ring.
#[test]
fn last_published_dedup_fires_once_per_distinct_job() {
    fn accept(current_job_id: JobId, last_published: &mut Option<JobId>, publishes: &mut u32) {
        if *last_published != Some(current_job_id) {
            *publishes += 1;
            *last_published = Some(current_job_id);
        }
    }

    let mut last_published: Option<JobId> = None;
    let mut publishes = 0;

    // Same job accepted repeatedly (e.g. repeated `client.drive()`
    // wakeups with no new job in between) publishes only the first time.
    accept(JobId(1), &mut last_published, &mut publishes);
    accept(JobId(1), &mut last_published, &mut publishes);
    accept(JobId(1), &mut last_published, &mut publishes);
    assert_eq!(publishes, 1);

    // A distinct job publishes again exactly once.
    accept(JobId(2), &mut last_published, &mut publishes);
    accept(JobId(2), &mut last_published, &mut publishes);
    assert_eq!(publishes, 2);
}

/// Client whose handshake always fails, ending the session at once.
struct FailingClient;

impl MiningClient for FailingClient {
    async fn connect<T: Read + Write>(&mut self, _transport: &mut T) -> Result<(), Error> {
        Err(Error::Transport)
    }

    fn submit(&mut self, _share: &Share<'_>) -> Result<(), Error> {
        Ok(())
    }

    async fn drive<T: Read + Write>(&mut self, _transport: &mut T) -> Result<DriveEvent, Error> {
        Err(Error::Transport)
    }

    fn accept<V: MiningClientVisitor>(&mut self, _visitor: &mut V) {}
}

/// Transport the failing client never touches.
struct NullTransport;

impl embedded_io_async::ErrorType for NullTransport {
    type Error = core::convert::Infallible;
}

impl Read for NullTransport {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

impl Write for NullTransport {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A session that ends empties its slot's work cell, so the dispatcher
/// stops handing the chip a dead pool's job.
#[test]
fn ended_session_clears_its_work() {
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    let asic: AsicSession = AsicSession::new();
    state.publish_work(asic.pool_handle(0), 0);
    let receiver = asic.work_receiver(0).expect("slot 0 has a receiver");
    assert!(receiver.contains_value());

    let result = embassy_futures::block_on(run_stratum_client(
        &mut FailingClient,
        &mut NullTransport,
        asic.pool_handle(0),
        &mut state,
        None,
        &mut NoExtension,
    ));

    assert_eq!(result, Err(Error::Transport));
    assert!(!receiver.contains_value());
}

/// An extension that records which foreign frames reached it.
struct Recording {
    frames: heapless::Vec<(u16, u8), 4>,
}

impl SessionExtension for Recording {
    fn connected(&mut self) {}

    fn frame(&mut self, frame: ExtensionFrame<'_>) {
        self.frames
            .push((frame.extension_type, frame.msg_type))
            .expect("fits capacity");
    }

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn tick(&mut self) {}

    fn outbound(&self) -> Option<ExtensionFrame<'_>> {
        None
    }

    fn outbound_result(&mut self, _result: Result<(), Error>) {}
}

/// The loop's visitor hands mining events to the state and foreign
/// frames to the extension, so neither sees the other's traffic.
#[test]
fn the_session_visitor_routes_extension_frames_and_mining_events_apart() {
    let mut state = StratumState::new(0);
    let mut recording = Recording {
        frames: heapless::Vec::new(),
    };
    let mut visitor = SessionVisitor {
        state: &mut state,
        extension: &mut recording,
    };
    visitor.visit_extension_frame(ExtensionFrame {
        is_channel_message: false,
        extension_type: 0x1234,
        msg_type: 7,
        payload: b"",
    });
    visitor.visit_new_job(sample_job(1));
    assert_eq!(recording.frames.as_slice(), &[(0x1234, 7)]);
    assert!(state.job_ring.current().is_some());
}

/// A session that carries no extension never wakes for one and never
/// has a frame to send, whatever reaches it.
#[test]
fn no_extension_has_no_deadline_and_nothing_to_send() {
    let mut none = NoExtension;
    none.connected();
    none.frame(ExtensionFrame {
        is_channel_message: false,
        extension_type: 0x1234,
        msg_type: 7,
        payload: b"x",
    });
    none.tick();
    assert_eq!(none.deadline(), None);
    assert!(none.outbound().is_none());
}

/// Scripted I/O events keep the session tests independent of wall-clock time.
#[derive(Clone, Copy)]
enum DriveStep {
    BlockWithSolutions,
    CleanJob,
    Flush,
    End,
}

const FIRST_NONCE: u32 = 11;
const SECOND_NONCE: u32 = 12;
const EXTENSION_SEND: u32 = 0;

/// A client with one outbound slot and a cancellable blocked write.
struct ScriptedClient<'a> {
    asic: &'a AsicSession,
    solution: Solution,
    steps: &'a [DriveStep],
    at: usize,
    occupied: bool,
    submitted: heapless::Vec<u32, 8>,
    send_error: Option<Error>,
}

impl MiningClient for ScriptedClient<'_> {
    async fn connect<T: Read + Write>(&mut self, _transport: &mut T) -> Result<(), Error> {
        Ok(())
    }

    fn submit(&mut self, share: &Share<'_>) -> Result<(), Error> {
        if self.occupied {
            return Err(Error::Busy);
        }
        self.occupied = true;
        self.submitted.push(share.nonce).expect("script fits");
        Ok(())
    }

    fn send_extension(&mut self, _frame: &ExtensionFrame<'_>) -> Result<(), Error> {
        if let Some(error) = self.send_error {
            return Err(error);
        }
        if self.occupied {
            return Err(Error::Busy);
        }
        self.occupied = true;
        self.submitted.push(EXTENSION_SEND).expect("script fits");
        Ok(())
    }

    async fn drive<T: Read + Write>(&mut self, _transport: &mut T) -> Result<DriveEvent, Error> {
        let step = *self.steps.get(self.at).expect("script exhausted");
        self.at += 1;
        match step {
            DriveStep::BlockWithSolutions => {
                for nonce in [FIRST_NONCE, SECOND_NONCE] {
                    assert!(self.asic.submit_solution(
                        0,
                        Solution {
                            nonce,
                            ..self.solution
                        }
                    ));
                }
                core::future::pending().await
            }
            DriveStep::CleanJob => Ok(DriveEvent::FrameReceived),
            DriveStep::Flush => {
                assert!(self.occupied);
                self.occupied = false;
                Ok(DriveEvent::OutboundFlushed)
            }
            DriveStep::End => Err(Error::Transport),
        }
    }

    fn accept<V: MiningClientVisitor>(&mut self, visitor: &mut V) {
        visitor.visit_new_job(MiningJob {
            clean_jobs: true,
            ..sample_job(2)
        });
    }
}

/// An extension that always exposes the next frame after a queue result.
struct QueuedExtension {
    remaining: usize,
    results: heapless::Vec<Result<(), Error>, 4>,
}

impl SessionExtension for QueuedExtension {
    fn connected(&mut self) {}
    fn frame(&mut self, _frame: ExtensionFrame<'_>) {}
    fn deadline(&self) -> Option<Instant> {
        None
    }
    fn tick(&mut self) {
        panic!("no deadline");
    }
    fn outbound(&self) -> Option<ExtensionFrame<'_>> {
        (self.remaining > 0).then_some(ExtensionFrame {
            extension_type: 0x1234,
            msg_type: 0,
            is_channel_message: false,
            payload: b"frame",
        })
    }
    fn outbound_result(&mut self, result: Result<(), Error>) {
        self.remaining -= 1;
        self.results.push(result).expect("script fits");
    }
}

/// Run the production loop through a blocked write and its scripted recovery.
fn run_script(steps: &[DriveStep]) -> heapless::Vec<u32, 8> {
    let asic = AsicSession::new();
    let mut state = StratumState::new(0);
    state.visit_new_job(sample_job(1));
    state.publish_work(asic.pool_handle(0), 0);
    let assignment = asic
        .work_receiver(0)
        .expect("receiver")
        .try_get()
        .expect("work");
    let mut client = ScriptedClient {
        asic: &asic,
        solution: Solution {
            assignment_id: assignment.assignment_id,
            nonce: FIRST_NONCE,
            ntime: assignment.ntime,
            version_bits: 0,
        },
        steps,
        at: 0,
        occupied: false,
        submitted: heapless::Vec::new(),
        send_error: None,
    };
    let mut extension = QueuedExtension {
        remaining: 2,
        results: heapless::Vec::new(),
    };
    let result = embassy_futures::block_on(run_stratum_client(
        &mut client,
        &mut NullTransport,
        asic.pool_handle(0),
        &mut state,
        None,
        &mut extension,
    ));
    assert_eq!(result, Err(Error::Transport));
    assert_eq!(extension.results.as_slice(), &[Ok(()), Ok(())]);
    assert!(asic.pool_handle(0).try_take_solution().is_none());
    client.submitted
}

#[test]
fn blocked_extension_writes_retain_shares_and_give_them_priority() {
    let sent = run_script(&[
        DriveStep::BlockWithSolutions,
        DriveStep::Flush,
        DriveStep::Flush,
        DriveStep::Flush,
        DriveStep::End,
    ]);
    assert_eq!(
        sent.as_slice(),
        &[EXTENSION_SEND, FIRST_NONCE, SECOND_NONCE, EXTENSION_SEND]
    );
}

#[test]
fn a_pending_share_is_revalidated_after_a_clean_job() {
    let sent = run_script(&[
        DriveStep::BlockWithSolutions,
        DriveStep::CleanJob,
        DriveStep::Flush,
        DriveStep::End,
    ]);
    assert_eq!(sent.as_slice(), &[EXTENSION_SEND, EXTENSION_SEND]);
}

#[test]
fn extension_queue_results_distinguish_busy_rejected_and_fatal() {
    let asic = AsicSession::new();
    let mut client = ScriptedClient {
        asic: &asic,
        solution: Solution {
            assignment_id: 0,
            nonce: 0,
            ntime: 0,
            version_bits: 0,
        },
        steps: &[],
        at: 0,
        occupied: false,
        submitted: heapless::Vec::new(),
        send_error: None,
    };
    for (error, expected) in [
        (None, Ok(())),
        (Some(Error::Busy), Ok(())),
        (Some(Error::NotImplemented), Ok(())),
        (Some(Error::BufferTooSmall), Ok(())),
        (Some(Error::Transport), Err(Error::Transport)),
        (Some(Error::Adapter), Err(Error::Adapter)),
    ] {
        client.occupied = false;
        client.send_error = error;
        let mut extension = QueuedExtension {
            remaining: 1,
            results: heapless::Vec::new(),
        };
        assert_eq!(offer_outbound(&mut client, &mut extension), expected);
        if error == Some(Error::Busy) {
            assert!(extension.outbound().is_some());
            assert!(extension.results.is_empty());
        } else {
            assert!(extension.outbound().is_none());
            assert_eq!(extension.results.as_slice(), &[error.map_or(Ok(()), Err)]);
        }
    }
}
