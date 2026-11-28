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

//! Tests for the scheduler-driven work dispatch: which pool slot's
//! cell a work tick reads, what the registry records, the empty-slot
//! skip and fallback, and the diff-1 share feedback into the
//! scheduler.
//!
//! The submit half of the routing (`process_solution` pushing into
//! `solutions[slot.session_slot]`) is exercised through a crafted
//! [`ChainSolution`] whose rebuilt header meets the diff-1 target,
//! which also covers the scheduler share accounting. What matters for
//! routing — that each registry entry pins the pool slot it was
//! dispatched from, unaffected by later scheduler picks — is covered
//! directly here.
//!
//! The command-arm and reply-protocol tests at the bottom drive
//! `handle_command` and `cmd_reply` directly with stub drivers: the
//! ticker wiring per result mode, the `Reset` desync drain, and the
//! orphaned-reply discard.
//!
//! The result-arm tests at the very end drive `Serial::run` itself,
//! since its `ResultEvent` match is inline in the loop body rather than
//! a function these tests could call directly. A scripted serial mock
//! and a driver stub built for that arm cover the push arm's
//! end-of-stream and line-error paths and a full poll-ticker cycle that
//! collects a solution through `process_solutions`, the published pool
//! weights reaching the scheduler, and a command racing a poll that is
//! genuinely in flight when it arrives.

use super::*;
use portable_atomic::AtomicU32;
use ubos_asic::{PollBatch, SolutionBatch, rolled_version};
use ubos_core::index::HashboardIndex;

use crate::consts::WORK_REGISTRY_SLOTS;
use crate::registry::WorkRegistry;

use embassy_futures::block_on;
use embassy_futures::join::join;
use embassy_futures::yield_now;
use stratum_client::AsicSession;
use ubos_config::DEVFEE;

use crate::scheduler::{FeeScheduler, RatioScheduler};

/// The poll mode of a chip read every sixty milliseconds.
const POLL: ResultMode = ResultMode::Poll {
    period: embassy_time::Duration::from_millis(60),
};
use ubos_asic::{
    BIP320_VERSION_MASK, BMM101_HASHBOARD, Bm13xxDriver, Chip, ChipDescriptor, ShareTarget,
};
use ubos_core::units::{FrequencyRange, Voltage};

use crate::stats::ChainStats;

/// Shared `ChainStats` for the tests that drive `Serial::run` and don't
/// care about its contents beyond having somewhere to report to.
static TEST_STATS: ChainStats = ChainStats::new();

/// Fixed-slot scheduler stub: `evaluate` returns the pinned slot when
/// it is available, otherwise the lowest available slot, so both the
/// pinned-slot path and dispatch's fallback path are exercisable.
/// `accumulate_share` just records which slot was credited.
struct Pinned {
    slot: usize,
    accumulated: heapless::Vec<usize, 8>,
}

impl Pinned {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            accumulated: heapless::Vec::new(),
        }
    }
}

impl<const N: usize> Scheduler<N> for Pinned {
    fn accumulate_share(&mut self, slot: usize) {
        self.accumulated
            .push(slot)
            .expect("test never accumulates more than 8 shares");
    }

    fn evaluate(&self, available: &[bool; N]) -> Option<usize> {
        if available.get(self.slot).copied().unwrap_or(false) {
            return Some(self.slot);
        }
        available.iter().position(|&is_available| is_available)
    }
}

/// Serial mock: captures dispatched frames, accepts any baud rate,
/// and answers reads with silence. The dispatch path only writes;
/// the `Read` bound comes along with the shared [`Transport`] impl.
struct MockSerial {
    written: heapless::Vec<u8, 512>,
}

impl MockSerial {
    fn new() -> Self {
        Self {
            written: heapless::Vec::new(),
        }
    }
}

impl embedded_io::ErrorType for MockSerial {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Write for MockSerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.written
            .extend_from_slice(buf)
            .expect("mock buffer fits a work frame");
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_io_async::Read for MockSerial {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}

impl BaudRate for MockSerial {
    fn set_baudrate(&mut self, _baud: u32) -> Result<(), ErrorKind> {
        Ok(())
    }
}

fn assignment(assignment_id: u32) -> WorkAssignment {
    WorkAssignment {
        assignment_id,
        prev_hash: [0x11; 32],
        merkle_root: [0x33; 32],
        target: [0x22; 32],
        version: 0x2000_0000,
        version_mask: 0,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
    }
}

/// Genesis block header fields, in the internal byte order
/// [`solution_hash`] expects. A mined block always meets diff-1 — it
/// is the easiest possible target — so genesis's known
/// `(header, nonce)` pair is a real chip solution that passes the
/// diff-1 check without brute-forcing one in the test.
const GENESIS_MERKLE_ROOT: [u8; 32] = [
    0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e, 0x67, 0x76, 0x8f, 0x61,
    0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32, 0x3a, 0x9f, 0xb8, 0xaa, 0x4b, 0x1e, 0x5e, 0x4a,
];

/// The genesis block's nonce as a [`ChainSolution`] for work ID 0,
/// with no version rolling and no ntime roll.
const GENESIS_SOLUTION: ChainSolution = ChainSolution {
    work_id: 0,
    nonce: 0x7C2B_AC1D,
    ntime_offset: 0,
    version_bits: 0,
};

/// A `WorkAssignment` matching the genesis block header. The
/// all-zero pool target guarantees the pool-target check in
/// `process_solution` fails, so the test isolates the scheduler
/// credit from the submit path.
fn genesis_assignment() -> WorkAssignment {
    WorkAssignment {
        assignment_id: 1,
        prev_hash: [0u8; 32],
        merkle_root: GENESIS_MERKLE_ROOT,
        target: [0u8; 32],
        version: 1,
        version_mask: 0,
        nbits: 0x1d00_ffff,
        ntime: 1_231_006_505,
    }
}

/// Per-test dispatch harness: one `AsicSession<N>`, its receivers,
/// the chain driver, a `Pinned` scheduler stub, and the counters
/// `dispatch_work` mutates. The actor is the single-chain one, so the
/// version mask is never partitioned away from a test's assignment.
struct Harness<'a, const N: usize> {
    actor: Serial,
    session: &'a AsicSession<N>,
    receivers: [DynReceiver<'a, WorkAssignment>; N],
    serial: MockSerial,
    driver: Bm13xxDriver,
    scheduler: Pinned,
    registry: WorkRegistry,
    last_version_mask: Option<u32>,
}

impl<'a, const N: usize> Harness<'a, N> {
    fn new(session: &'a AsicSession<N>, pinned_slot: usize) -> Self {
        let driver = Bm13xxDriver::new(Chip::Bm1362);
        let registry = WorkRegistry::new(driver.work_slots());
        Self {
            actor: Serial::new(HashboardIndex(0), 1),
            session,
            receivers: core::array::from_fn(|slot| {
                session
                    .work_receiver(slot)
                    .expect("test session has a free receiver per slot")
            }),
            serial: MockSerial::new(),
            driver,
            scheduler: Pinned::new(pinned_slot),
            registry,
            last_version_mask: None,
        }
    }

    fn dispatch(&mut self) -> Option<usize> {
        block_on(self.actor.dispatch_work(
            &mut self.serial,
            &self.scheduler,
            &mut self.driver,
            &mut self.registry,
            &mut self.receivers,
            &mut self.last_version_mask,
        ))
    }

    fn process_solution(&mut self, solution: &ChainSolution, difficulty: &[u8; 32]) {
        Serial::process_solution(
            solution,
            &self.registry,
            self.session,
            &mut self.scheduler,
            difficulty,
            &TEST_STATS,
        );
    }

    /// Independently rehash a solution against the registry and verify
    /// it meets the given difficulty target.
    fn assert_solution_meets_floor(&self, solution: &ChainSolution, difficulty: &[u8; 32]) {
        let slot = self
            .registry
            .get(solution.work_id)
            .expect("solution's work_id must map to a populated registry slot");
        let version = rolled_version(
            slot.assignment.version,
            solution.version_bits,
            BIP320_VERSION_MASK,
        );
        let ntime = slot.assignment.ntime.wrapping_add(solution.ntime_offset);
        let hash = solution_hash(
            version,
            &slot.assignment.prev_hash,
            &slot.assignment.merkle_root,
            ntime,
            slot.assignment.nbits,
            solution.nonce,
        );
        assert!(
            hash_meets_target(&hash, difficulty),
            "independently rehashed solution must meet the difficulty target"
        );
    }
}

#[test]
fn dispatch_reads_the_pinned_slot() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));

    let mut harness = Harness::new(&session, 1);
    assert_eq!(harness.dispatch(), Some(1));

    let slot = harness.registry.get(0).expect("work dispatched");
    assert_eq!(slot.assignment.assignment_id, 20);
    assert_eq!(slot.session_slot, 1);
    assert!(!harness.serial.written.is_empty(), "frame reached the chip");
}

#[test]
fn dispatch_skips_the_tick_when_no_slot_has_work() {
    let session: AsicSession<2> = AsicSession::new();
    // Neither slot has published anything: the pinned slot is empty
    // and there is no other slot to fall back to.
    let mut harness = Harness::new(&session, 1);
    assert_eq!(harness.dispatch(), None);

    assert!(harness.registry.get(0).is_none());
    assert!(harness.serial.written.is_empty());

    // The skipped tick consumed no work ID: the next dispatch takes the first.
    session.pool_handle(1).publish_work(assignment(20));
    harness.dispatch();
    assert!(harness.registry.get(0).is_some());
}

#[test]
fn dispatch_falls_back_to_an_available_slot_when_the_pinned_slot_is_empty() {
    let session: AsicSession<2> = AsicSession::new();
    // Slot 0 has work; the pinned slot 1 has published nothing.
    session.pool_handle(0).publish_work(assignment(10));

    let mut harness = Harness::new(&session, 1);
    assert_eq!(harness.dispatch(), Some(0));

    let slot = harness.registry.get(0).expect("fallback dispatch");
    assert_eq!(slot.assignment.assignment_id, 10);
    assert_eq!(slot.session_slot, 0);
    assert!(!harness.serial.written.is_empty(), "frame reached the chip");
}

#[test]
fn dispatch_skips_a_tick_when_the_pinned_slot_was_cleared() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(0).clear_work();

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    assert!(harness.registry.get(0).is_none());
    assert!(harness.serial.written.is_empty());
}

#[test]
fn registry_pins_the_dispatch_time_slot_across_a_pin_change() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(0).publish_work(assignment(10));
    session.pool_handle(1).publish_work(assignment(20));

    let mut harness = Harness::new(&session, 0);
    // First dispatch while slot 0 is pinned, then change the pin
    // mid-flight and dispatch again.
    harness.dispatch();
    harness.scheduler.slot = 1;
    harness.dispatch();

    let first = harness.registry.get(0).expect("first dispatch");
    let second = harness.registry.get(1).expect("second dispatch");
    // The first entry still routes to slot 0: a solution arriving for
    // it after the pin change goes back to the session that owns the
    // work, not to the newly pinned one.
    assert_eq!(first.session_slot, 0);
    assert_eq!(first.assignment.assignment_id, 10);
    assert_eq!(second.session_slot, 1);
    assert_eq!(second.assignment.assignment_id, 20);
}

#[test]
fn process_solution_credits_the_scheduler_on_a_diff_1_share() {
    let session: AsicSession<1> = AsicSession::new();
    session.pool_handle(0).publish_work(genesis_assignment());

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    let chain_stats = ChainStats::new();
    Serial::process_solution(
        &GENESIS_SOLUTION,
        &harness.registry,
        harness.session,
        &mut harness.scheduler,
        &ShareTarget::DIFF_1,
        &chain_stats,
    );

    assert_eq!(harness.scheduler.accumulated.as_slice(), &[0]);
}

#[test]
fn process_solution_bumps_the_originating_slots_pool_work_share_counter() {
    let session: AsicSession<2> = AsicSession::new();
    session.pool_handle(1).publish_work(genesis_assignment());

    let mut harness = Harness::new(&session, 1);
    harness.dispatch();

    let chain_stats = ChainStats::new();
    Serial::process_solution(
        &GENESIS_SOLUTION,
        &harness.registry,
        harness.session,
        &mut harness.scheduler,
        &ShareTarget::DIFF_1,
        &chain_stats,
    );

    assert_eq!(
        chain_stats.pool_shares(1).get(),
        1,
        "the originating slot's counter records the diff-1 share"
    );
    assert_eq!(
        chain_stats.pool_shares(2).get(),
        0,
        "an unrelated slot's counter is untouched"
    );
}

/// A difficulty harder than [`ShareTarget::DIFF_1`]: it clears the
/// target's leading word down to a single low byte, `0x10` at index
/// 26. The genesis solution's hash carries `0x19` at that same index
/// (with every more-significant byte zero on both sides), so it fails
/// this difficulty while still meeting `ShareTarget::DIFF_1` itself and
/// any pool target at or looser than that.
const HARDER_THAN_DIFF_1_FLOOR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[26] = 0x10;
    bytes
};

#[test]
fn process_solution_rejects_a_solution_between_the_pool_target_and_a_harder_floor() {
    let session: AsicSession<1> = AsicSession::new();
    let assignment = WorkAssignment {
        target: *ShareTarget::DIFF_1,
        ..genesis_assignment()
    };
    session.pool_handle(0).publish_work(assignment);

    let mut harness = Harness::new(&session, 0);
    harness.dispatch();

    harness.process_solution(&GENESIS_SOLUTION, &HARDER_THAN_DIFF_1_FLOOR);

    assert!(
        harness.scheduler.accumulated.is_empty(),
        "a solution below the driver's own difficulty must not be credited"
    );
    assert!(
        session.pool_handle(0).try_take_solution().is_none(),
        "a solution below the difficulty must never reach the pool queue either"
    );

    harness.assert_solution_meets_floor(&GENESIS_SOLUTION, &ShareTarget::DIFF_1);

    harness.process_solution(&GENESIS_SOLUTION, &ShareTarget::DIFF_1);

    assert_eq!(
        harness.scheduler.accumulated.as_slice(),
        &[0],
        "the same solution against the loosened difficulty is credited"
    );
    let submitted = session
        .pool_handle(0)
        .try_take_solution()
        .expect("the solution reaches the pool queue once the difficulty no longer blocks it");
    assert_eq!(submitted.nonce, GENESIS_SOLUTION.nonce);
    assert_eq!(submitted.assignment_id, genesis_assignment().assignment_id);
}

/// Capability constants for [`StubDriver`]. The values are arbitrary:
/// the command arm never reads them.
static POLL_CHIP: ChipDescriptor = ChipDescriptor {
    asic_model: "TEST",
    core_count: 1,
    max_chip_voltage: Voltage::from_mv(1200),
    frequency_step: Frequency::from_mhz(5),
    frequency_range: FrequencyRange::new(Frequency::from_mhz(100), Frequency::from_mhz(400)),
    rolls_ntime: false,
};

/// Minimal [`ChainDriver`] stub for the command-arm tests, with a
/// selectable result mode. Wire operations are no-ops; `reset` calls
/// are counted and `take_desync` serves a scripted tally that `reset`
/// zeroes, so the drain-before-reset ordering in the `Reset` arm is
/// observable. The real `Bm13xxDriver` cannot stand in here: its
/// bring-up paths await `Timer::after`, whose timer-queue plumbing a
/// host test binary without an embassy executor cannot link.
struct StubDriver {
    mode: ResultMode,
    resets: usize,
    desync: u32,
    enumerate_count: u8,
    configure_err: Option<ChipError>,
    configured_frequency: Frequency,
    set_frequency_err: Option<ChipError>,
    /// The last job `send_work` was handed, for the dispatch tests that
    /// read the job a chain built rather than the frame it wrote.
    last_job: Option<WorkJob>,
}

impl StubDriver {
    fn new(mode: ResultMode, desync: u32) -> Self {
        Self {
            mode,
            resets: 0,
            desync,
            enumerate_count: 0,
            configure_err: None,
            configured_frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            set_frequency_err: None,
            last_job: None,
        }
    }

    /// Chip count `reset_and_enumerate` reports back.
    fn with_enumerate_count(mut self, count: u8) -> Self {
        self.enumerate_count = count;
        self
    }

    /// Error `configure` returns instead of succeeding.
    fn with_configure_err(mut self, err: ChipError) -> Self {
        self.configure_err = Some(err);
        self
    }

    /// Error `set_frequency` returns instead of succeeding.
    fn with_set_frequency_err(mut self, err: ChipError) -> Self {
        self.set_frequency_err = Some(err);
        self
    }
}

impl ChainDriver for StubDriver {
    fn descriptor(&self) -> &'static ChipDescriptor {
        &POLL_CHIP
    }

    fn boot_uart_rate(&self) -> u32 {
        115_200
    }

    fn work_slots(&self) -> usize {
        1
    }

    async fn reset_and_enumerate(
        &mut self,
        _io: &mut impl Transport,
        _board: &HashboardDescriptor,
    ) -> u8 {
        self.enumerate_count
    }

    async fn configure(
        &mut self,
        _io: &mut impl Transport,
        _board: &HashboardDescriptor,
    ) -> Result<Frequency, ChipError> {
        match self.configure_err {
            Some(err) => Err(err),
            None => Ok(self.configured_frequency),
        }
    }

    async fn set_frequency(
        &mut self,
        _io: &mut impl Transport,
        _freq: Frequency,
    ) -> Result<(), ChipError> {
        match self.set_frequency_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn send_work(
        &mut self,
        _io: &mut impl Transport,
        job: &WorkJob,
    ) -> Result<(), ChipError> {
        self.last_job = Some(job.clone());
        Ok(())
    }

    fn result_mode(&self) -> ResultMode {
        self.mode
    }

    fn difficulty_target(&self) -> ShareTarget {
        ShareTarget::DIFF_1
    }

    fn ingest(&mut self, _bytes: &[u8]) -> SolutionBatch {
        SolutionBatch::new()
    }

    async fn poll_results(&mut self, _io: &mut impl Transport) -> PollBatch {
        PollBatch {
            solutions: SolutionBatch::new(),
            difficulty: self.difficulty_target(),
            progress: PollProgress::Complete,
        }
    }

    fn reset(&mut self) {
        self.resets += 1;
        self.desync = 0;
    }

    fn take_desync(&mut self) -> u32 {
        core::mem::take(&mut self.desync)
    }
}

/// The `Serial::run` locals the command arm mutates, bundled per test.
struct CommandState {
    work_ticker: Option<Ticker>,
    poll_cadence: Option<PollCadence>,
    registry: WorkRegistry,
}

impl CommandState {
    fn new() -> Self {
        Self {
            work_ticker: None,
            poll_cadence: None,
            registry: WorkRegistry::new(WORK_REGISTRY_SLOTS),
        }
    }
}

/// Run one command through `handle_command` and return the reply the
/// server signalled for it. The commands run here never ask whether
/// work is held, so the actor has no pool slots.
fn run_command(
    actor: &Serial,
    msg: SerialMessage,
    driver: &mut impl ChainDriver,
    state: &mut CommandState,
    chain_stats: &ChainStats,
) -> Reply {
    run_command_over(actor, msg, driver, state, &[], &Pinned::new(0), chain_stats)
}

/// Run one command through `handle_command` over `receivers` and
/// `scheduler`, and return the reply the server signalled for it.
fn run_command_over<const N: usize>(
    actor: &Serial,
    msg: SerialMessage,
    driver: &mut impl ChainDriver,
    state: &mut CommandState,
    receivers: &[DynReceiver<'_, WorkAssignment>; N],
    scheduler: &impl Scheduler<N>,
    chain_stats: &ChainStats,
) -> Reply {
    let mut serial = MockSerial::new();
    let flow = block_on(actor.handle_command(
        0,
        msg,
        &mut serial,
        driver,
        &BMM101_HASHBOARD,
        &mut state.work_ticker,
        &mut state.poll_cadence,
        &mut state.registry,
        receivers,
        scheduler,
        chain_stats,
    ));
    assert!(flow.is_continue(), "only Terminate breaks the loop");
    let (seq, reply) = actor.reply.try_take().expect("every command is answered");
    assert_eq!(seq, 0, "the reply carries the command's tag");
    reply
}

#[test]
fn start_sending_work_arms_the_poll_cadence_only_for_a_poll_driver() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();

    let mut state = CommandState::new();
    let mut poll = StubDriver::new(POLL, 0);
    let period = Duration::from_millis(100);
    let reply = run_command(
        &actor,
        SerialMessage::StartSendingWork(period),
        &mut poll,
        &mut state,
        &chain_stats,
    );
    assert!(matches!(reply, Reply::Unit(Ok(()))));
    assert!(state.work_ticker.is_some());
    assert!(state.poll_cadence.is_some());

    let mut state = CommandState::new();
    let mut push = StubDriver::new(ResultMode::Push, 0);
    let reply = run_command(
        &actor,
        SerialMessage::StartSendingWork(period),
        &mut push,
        &mut state,
        &chain_stats,
    );
    assert!(matches!(reply, Reply::Unit(Ok(()))));
    assert!(state.work_ticker.is_some());
    assert!(state.poll_cadence.is_none(), "push drivers poll nothing");
}

#[test]
fn stop_sending_work_clears_the_poll_cadence_armed_by_start_sending_work() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let mut state = CommandState::new();
    let mut poll = StubDriver::new(POLL, 0);
    let period = Duration::from_millis(100);

    let reply = run_command(
        &actor,
        SerialMessage::StartSendingWork(period),
        &mut poll,
        &mut state,
        &chain_stats,
    );
    assert!(matches!(reply, Reply::Unit(Ok(()))));
    assert!(state.work_ticker.is_some());
    assert!(state.poll_cadence.is_some());

    let reply = run_command(
        &actor,
        SerialMessage::StopSendingWork,
        &mut poll,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(reply, Reply::Unit(Ok(()))));
    assert!(state.work_ticker.is_none());
    assert!(
        state.poll_cadence.is_none(),
        "stop must clear the poll ticker a poll driver armed"
    );
}

#[test]
fn reset_drains_the_desync_tally_before_clearing_the_driver() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let mut state = CommandState::new();
    state.work_ticker = Some(Ticker::every(Duration::from_millis(50)));
    state.poll_cadence = Some(PollCadence::every(Duration::from_millis(50)));
    state
        .registry
        .place(assignment(1), 0, JobVersion::pinned(0));
    let mut driver = StubDriver::new(POLL, 37);

    let reply = run_command(
        &actor,
        SerialMessage::Reset,
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(reply, Reply::Unit(Ok(()))));
    assert_eq!(driver.resets, 1);
    // The tally accumulated before the reset reaches the chain's stats
    // instead of dying with the driver state.
    assert_eq!(chain_stats.desync_bytes(), 37);
    assert!(state.work_ticker.is_none());
    assert!(state.poll_cadence.is_none());
    assert!(state.registry.get(0).is_none());
    assert_eq!(
        state
            .registry
            .place(assignment(2), 0, JobVersion::pinned(0)),
        0,
        "the work IDs start over"
    );
}

#[test]
fn reset_enumerate_replies_with_the_drivers_chip_count_and_drains_desync() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let mut state = CommandState::new();
    let mut driver = StubDriver::new(POLL, 5).with_enumerate_count(3);

    let reply = run_command(
        &actor,
        SerialMessage::ResetEnumerate,
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(reply, Reply::Count(3)));
    assert_eq!(chain_stats.desync_bytes(), 5);
    assert_eq!(
        chain_stats.chips(),
        0,
        "the count goes back to the miner, which is the only writer of it"
    );
}

#[test]
fn configure_records_the_frequency_returned_by_the_driver() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    chain_stats.set_frequency(Frequency::from_mhz(550));
    let mut state = CommandState::new();
    let mut driver = StubDriver::new(POLL, 0);
    let configured = Frequency::from_mhz(250);
    driver.configured_frequency = configured;
    assert_ne!(configured, BMM101_HASHBOARD.chip.frequency_range.min());

    let reply = run_command(
        &actor,
        SerialMessage::Configure,
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(reply, Reply::Freq(Ok(()))));
    assert_eq!(chain_stats.frequency(), configured);
}

#[test]
fn configure_propagates_the_drivers_baud_switch_error_and_still_drains_desync() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let previous = Frequency::from_mhz(550);
    chain_stats.set_frequency(previous);
    let mut state = CommandState::new();
    let mut driver = StubDriver::new(POLL, 7).with_configure_err(ChipError::BaudSwitch(3_000_000));

    let reply = run_command(
        &actor,
        SerialMessage::Configure,
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(
        reply,
        Reply::Freq(Err(ChipError::BaudSwitch(3_000_000)))
    ));
    // The tally still reaches the chain's stats on a failed bring-up: the
    // chip has already moved baud and needs a reset either way.
    assert_eq!(chain_stats.desync_bytes(), 7);
    assert_eq!(chain_stats.frequency(), previous);
}

#[test]
fn set_frequency_replies_with_a_unit_result_on_success() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let mut state = CommandState::new();
    let mut driver = StubDriver::new(POLL, 0);

    let reply = run_command(
        &actor,
        SerialMessage::SetFrequency(Frequency::from_mhz(400)),
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(reply, Reply::Freq(Ok(()))));
}

#[test]
fn set_frequency_propagates_the_drivers_unsupported_frequency_error() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let chain_stats = ChainStats::new();
    let mut state = CommandState::new();
    let freq = Frequency::from_mhz(900);
    let mut driver =
        StubDriver::new(POLL, 0).with_set_frequency_err(ChipError::UnsupportedFrequency(freq));

    let reply = run_command(
        &actor,
        SerialMessage::SetFrequency(freq),
        &mut driver,
        &mut state,
        &chain_stats,
    );

    assert!(matches!(
        reply,
        Reply::Freq(Err(ChipError::UnsupportedFrequency(f))) if f == freq
    ));
}

#[test]
fn cmd_reply_discards_an_orphaned_predecessor_reply() {
    let actor = Serial::new(HashboardIndex(0), 1);
    let caller = actor.cmd_reply(SerialMessage::StopSendingWork);
    let server = async {
        let (seq, _msg) = actor.cmd.receive().await;
        // Answer the cancelled predecessor first: a stale tag carrying
        // a variant the pending command never produces.
        actor.reply.signal((seq.wrapping_sub(1), Reply::Count(9)));
        // Let the caller observe and discard the orphan before the
        // real answer overwrites the signal slot.
        yield_now().await;
        actor.reply.signal((seq, Reply::Unit(Ok(()))));
    };

    let (reply, ()) = block_on(join(caller, server));

    assert!(matches!(reply, Reply::Unit(Ok(()))));
}

/// Ask the actor whether `scheduler` would dispatch from the slots of
/// `receivers` that hold a job.
fn work_is_available<const N: usize>(
    receivers: &[DynReceiver<'_, WorkAssignment>; N],
    scheduler: &impl Scheduler<N>,
) -> bool {
    let reply = run_command_over(
        &Serial::new(HashboardIndex(0), 1),
        SerialMessage::WorkAvailable,
        &mut StubDriver::new(ResultMode::Push, 0),
        &mut CommandState::new(),
        receivers,
        scheduler,
        &ChainStats::new(),
    );
    let Reply::Flag(available) = reply else {
        panic!("a work query is answered with a flag")
    };
    available
}

#[test]
fn work_is_available_when_a_slot_the_scheduler_serves_holds_a_job() {
    let session = AsicSession::<2>::new();
    let receivers = core::array::from_fn(|slot| {
        session
            .work_receiver(slot)
            .expect("test session has a free receiver per slot")
    });
    // Slot 0 has weight 1 and slot 1 is a backup, so both are served.
    let scheduler = RatioScheduler::<2>::new();
    assert!(!work_is_available(&receivers, &scheduler));

    session.pool_handle(1).publish_work(assignment(1));
    assert!(work_is_available(&receivers, &scheduler));

    session.pool_handle(1).clear_work();
    session.pool_handle(0).publish_work(assignment(2));
    assert!(work_is_available(&receivers, &scheduler));

    session.pool_handle(0).clear_work();
    assert!(!work_is_available(&receivers, &scheduler));
}

#[test]
fn work_is_not_available_while_only_the_fee_slot_holds_a_job() {
    if !DEVFEE {
        return;
    }
    let session = AsicSession::<2>::new();
    let receivers = core::array::from_fn(|slot| {
        session
            .work_receiver(slot)
            .expect("test session has a free receiver per slot")
    });
    // The fee takes the last slot of a two-slot board.
    let scheduler = FeeScheduler::<2>::new(20);

    session.pool_handle(1).publish_work(assignment(1));
    assert!(!work_is_available(&receivers, &scheduler));

    session.pool_handle(0).publish_work(assignment(2));
    assert!(work_is_available(&receivers, &scheduler));

    session.pool_handle(0).clear_work();
    assert!(
        !work_is_available(&receivers, &scheduler),
        "the fee job that remains is not a pool to mine for"
    );
}

// ---------------------------------------------------------------------------
// Result-arm tests: drive `Serial::run` itself
//
// `run`'s match on `ResultEvent` is inline in the loop body, not a
// function these tests could call directly, so each test here joins a
// command-issuing future against `run` itself. `ScriptedSerial` scripts
// one read outcome and then goes quiet; `ResultDriver` is the
// `ChainDriver` stub these tests drive it with.
//
// These are the first tests in this file to call `run` itself rather
// than `dispatch_work` or `handle_command` directly. Once a test
// references `run`, its `info!`/`warn!`/`error!` call sites are linked
// into the host test binary and need a `defmt` global logger to resolve.
// No board's global logger is reachable from this crate's host build,
// so `TestLogger` below is one built for this binary alone; it
// discards every frame, since no test reads log output back.
// ---------------------------------------------------------------------------

#[cfg(feature = "defmt")]
mod test_logger {
    use core::cell::UnsafeCell;

    use portable_atomic::{AtomicBool, Ordering};

    #[defmt::global_logger]
    struct TestLogger;

    // `defmt` also needs a timestamp source wired in alongside the
    // logger; every timestamp in this binary's log frames reads zero,
    // since nothing decodes them.
    defmt::timestamp!("{=u32}", 0);

    /// State guarded by the critical section held between `acquire()`
    /// and `release()`. `cargo test`'s default runner spreads tests
    /// across OS threads, so that section is what keeps two tests'
    /// concurrent `Serial::run` calls from logging over one another.
    struct LoggerState {
        taken: AtomicBool,
        restore: UnsafeCell<critical_section::RestoreState>,
    }

    // Safety: every access to `restore` happens while the critical
    // section acquired in `acquire()` is held, so it is never touched
    // from two threads at once despite the raw pointer inside the cell.
    unsafe impl Sync for LoggerState {}

    static STATE: LoggerState = LoggerState {
        taken: AtomicBool::new(false),
        restore: UnsafeCell::new(critical_section::RestoreState::invalid()),
    };

    unsafe impl defmt::Logger for TestLogger {
        fn acquire() {
            // Safety: paired with exactly one `critical_section::release`
            // in `release()`, matching this trait's contract.
            let restore = unsafe { critical_section::acquire() };
            assert!(
                !STATE.taken.load(Ordering::Relaxed),
                "defmt logger acquired twice in the same critical section"
            );
            STATE.taken.store(true, Ordering::Relaxed);
            // Safety: the critical section just acquired above serialises
            // this write against every other holder.
            unsafe {
                *STATE.restore.get() = restore;
            }
        }

        unsafe fn flush() {}

        unsafe fn release() {
            STATE.taken.store(false, Ordering::Relaxed);
            // Safety: the critical section held since `acquire()`
            // serialises this read against every other holder.
            let restore = unsafe { *STATE.restore.get() };
            // Safety: paired with the `critical_section::acquire` in
            // `acquire()`.
            unsafe {
                critical_section::release(restore);
            }
        }

        unsafe fn write(_bytes: &[u8]) {}
    }
}

/// One scripted read outcome for [`ScriptedSerial`]. Consumed by the
/// first read call; every read after that pends forever, mirroring a
/// live but quiet line so the actor's command arm can win the select.
#[derive(Clone, Copy)]
enum ReadStep {
    /// A zero-byte read: the push arm's end-of-stream signal.
    Eof,
    /// A line-level read error of this kind.
    Error(ErrorKind),
}

/// Serial mock that yields one scripted read outcome and then goes
/// quiet, for driving `Serial::run`'s result arm end to end. Writes
/// always succeed and the baud switch is a no-op, matching `MockSerial`.
struct ScriptedSerial {
    step: Option<ReadStep>,
}

impl ScriptedSerial {
    fn new(step: ReadStep) -> Self {
        Self { step: Some(step) }
    }

    /// A line with nothing scripted. Every read pends forever, so only
    /// the command arm can make the actor's select resolve.
    fn quiet() -> Self {
        Self { step: None }
    }
}

impl embedded_io::ErrorType for ScriptedSerial {
    type Error = ErrorKind;
}

impl embedded_io_async::Read for ScriptedSerial {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self.step.take() {
            Some(ReadStep::Eof) => Ok(0),
            Some(ReadStep::Error(kind)) => Err(kind),
            None => core::future::pending::<Result<usize, Self::Error>>().await,
        }
    }
}

impl embedded_io_async::Write for ScriptedSerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl BaudRate for ScriptedSerial {
    fn set_baudrate(&mut self, _baud: u32) -> Result<(), ErrorKind> {
        Ok(())
    }
}

/// One rendezvous flag for gating a `poll_results` call from a test; see
/// [`ResultDriver::with_poll_rendezvous`].
type PollGate = Signal<CriticalSectionRawMutex, ()>;

/// What `ResultDriver::send_work` records, in `'static` cells a test
/// reads after `run` drops the driver.
#[derive(Clone, Copy)]
struct SendWorkTracking {
    /// Work ID of the last job sent.
    last_work_id: &'static AtomicU32,
    /// `ntime` of the last job sent, which the clamp test uses to name
    /// the assignment the job came from.
    last_ntime: &'static AtomicU32,
    /// Number of `send_work` calls.
    calls: &'static AtomicU32,
}

/// Chain-driver stub for the result-arm tests. `line_errors` and
/// `ingest_calls` are `&'static` counters rather than owned fields so a
/// test can still read them after `run` consumes and drops the driver.
struct ResultDriver {
    mode: ResultMode,
    line_errors: &'static AtomicU32,
    ingest_calls: &'static AtomicU32,
    /// Solution `poll_results` hands back on its first call. Cleared
    /// after that call, so a second poll tick returns an empty batch.
    poll_solution: Option<ChainSolution>,
    hardware_difficulty: u64,
    /// `work_slots()` to report. Defaults to 1; overridable so a test can
    /// drive `run`'s registry-size clamp with a driver that claims more
    /// slots than the registry holds.
    work_slots: usize,
    /// Counters the work-slots clamp test reads after `run` drops the
    /// driver.
    send_work_tracking: Option<SendWorkTracking>,
    /// Pause after the result enters the local batch, so a work tick or
    /// command that cancels the call would lose an already collected find.
    poll_rendezvous: Option<(&'static PollGate, &'static PollGate)>,
}

impl ResultDriver {
    fn new(
        mode: ResultMode,
        line_errors: &'static AtomicU32,
        ingest_calls: &'static AtomicU32,
    ) -> Self {
        Self {
            mode,
            line_errors,
            ingest_calls,
            poll_solution: None,
            hardware_difficulty: 1,
            work_slots: 1,
            send_work_tracking: None,
            poll_rendezvous: None,
        }
    }

    /// Return `solution` from the first `poll_results` call.
    fn with_poll_solution(mut self, solution: ChainSolution) -> Self {
        self.poll_solution = Some(solution);
        self
    }

    /// Report `slots` from `work_slots()` instead of the default of 1.
    fn with_work_slots(mut self, slots: usize) -> Self {
        self.work_slots = slots;
        self
    }

    /// Record every `send_work` call into `tracking`.
    fn with_send_work_tracking(mut self, tracking: SendWorkTracking) -> Self {
        self.send_work_tracking = Some(tracking);
        self
    }

    /// Pause a collected chunk while work ticks and commands arrive.
    fn with_poll_rendezvous(mut self, entered: &'static PollGate, gate: &'static PollGate) -> Self {
        self.poll_rendezvous = Some((entered, gate));
        self
    }
}

impl ChainDriver for ResultDriver {
    fn descriptor(&self) -> &'static ChipDescriptor {
        // `run` asserts the board and driver agree on the chip; every
        // test in this file that drives `run` passes `&BMM101_HASHBOARD`,
        // so the driver must report the exact static its `chip` field
        // points to rather than the unrelated `POLL_CHIP` stub.
        BMM101_HASHBOARD.chip
    }

    fn boot_uart_rate(&self) -> u32 {
        115_200
    }

    fn work_slots(&self) -> usize {
        self.work_slots
    }

    async fn reset_and_enumerate(
        &mut self,
        _io: &mut impl Transport,
        _board: &HashboardDescriptor,
    ) -> u8 {
        0
    }

    async fn configure(
        &mut self,
        _io: &mut impl Transport,
        board: &HashboardDescriptor,
    ) -> Result<Frequency, ChipError> {
        Ok(board.chip.frequency_range.min())
    }

    async fn set_frequency(
        &mut self,
        _io: &mut impl Transport,
        _freq: Frequency,
    ) -> Result<(), ChipError> {
        Ok(())
    }

    async fn send_work(
        &mut self,
        _io: &mut impl Transport,
        job: &WorkJob,
    ) -> Result<(), ChipError> {
        if let Some(tracking) = self.send_work_tracking {
            tracking.last_work_id.store(job.work_id, Ordering::Relaxed);
            tracking.last_ntime.store(job.ntime, Ordering::Relaxed);
            tracking.calls.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn result_mode(&self) -> ResultMode {
        self.mode
    }

    fn difficulty_target(&self) -> ShareTarget {
        ShareTarget::from_pow2_difficulty(self.hardware_difficulty)
    }

    fn ingest(&mut self, _bytes: &[u8]) -> SolutionBatch {
        self.ingest_calls.fetch_add(1, Ordering::Relaxed);
        SolutionBatch::new()
    }

    async fn poll_results(&mut self, _io: &mut impl Transport) -> PollBatch {
        let mut batch = SolutionBatch::new();
        if let Some(solution) = self.poll_solution.take() {
            batch
                .push(solution)
                .expect("a fresh batch always has room for one solution");
        }
        if let Some((entered, gate)) = self.poll_rendezvous {
            entered.signal(());
            gate.wait().await;
        }
        PollBatch {
            solutions: batch,
            difficulty: ShareTarget::DIFF_1,
            progress: PollProgress::Complete,
        }
    }

    fn reset(&mut self) {}

    fn line_error(&mut self) {
        self.line_errors.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command() {
    // Serializes against every other test that drives `Serial::run`,
    // since `run` writes through the one global logger under the `defmt`
    // feature, and two runs live at once would interleave its frames.
    critical_section::with(|_| {
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);

        let actor = Serial::new(HashboardIndex(0), 1);
        let driver = ResultDriver::new(ResultMode::Push, &LINE_ERRORS, &INGEST_CALLS);
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::new(ReadStep::Eof);

        let result = block_on(actor.run_with(
            crate::fixture::park(),
            serial,
            &SESSION,
            &BMM101_HASHBOARD,
            driver,
            scheduler,
            &TEST_STATS,
        ));

        assert!(
            matches!(result, Err(ErrorKind::BrokenPipe)),
            "a zero-byte push read is end-of-stream, not a quiet line"
        );
        assert_eq!(
            INGEST_CALLS.load(Ordering::Relaxed),
            0,
            "the empty read must never reach ingest as if it carried a frame"
        );

        // A supervisor that restarts `run` on the same actor after this EOF
        // exit still gets a working command round trip: the caller mutex,
        // reply signal, and command channel were left in a clean state.
        let driver = ResultDriver::new(ResultMode::Push, &LINE_ERRORS, &INGEST_CALLS);
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::quiet();

        let (terminate_result, run_result) = block_on(join(
            actor.terminate(),
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(terminate_result, Ok(())));
        assert!(matches!(run_result, Ok(())));
    });
}

#[test]
fn read_line_error_recovers_the_parser_and_the_actor_keeps_serving_commands() {
    // See the comment on
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);

        let actor = Serial::new(HashboardIndex(0), 1);
        let driver = ResultDriver::new(ResultMode::Push, &LINE_ERRORS, &INGEST_CALLS);
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::new(ReadStep::Error(ErrorKind::Other));

        let caller = async {
            while LINE_ERRORS.load(Ordering::Relaxed) == 0 {
                yield_now().await;
            }
            actor.terminate().await
        };
        let (terminate_result, run_result) = block_on(join(
            caller,
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(terminate_result, Ok(())));
        assert!(
            matches!(run_result, Ok(())),
            "a line-level fault must not end the run the way end-of-stream does"
        );
        assert_eq!(
            LINE_ERRORS.load(Ordering::Relaxed),
            1,
            "the fault must reset the driver's buffered partial frame"
        );
        assert_eq!(
            INGEST_CALLS.load(Ordering::Relaxed),
            0,
            "a faulted read must never be decoded as data"
        );
    });
}

#[test]
fn a_poll_collects_a_solution_through_process_solutions_without_calling_ingest() {
    // See the comment on
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        const HARDWARE_DIFFICULTY: u64 = 1 << 63;
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);

        // The pool target is the diff-1 difficulty itself, so the genesis header
        // GENESIS_SOLUTION solves also clears the pool target and the
        // solution reaches the session's solution channel rather than only
        // the scheduler's diff-1 credit.
        let assignment = WorkAssignment {
            target: *ShareTarget::DIFF_1,
            ..genesis_assignment()
        };
        SESSION.pool_handle(0).publish_work(assignment);

        let actor = Serial::new(HashboardIndex(0), 1);
        let mut driver = ResultDriver::new(POLL, &LINE_ERRORS, &INGEST_CALLS)
            .with_poll_solution(GENESIS_SOLUTION);
        // The chip has moved to a harder filter while its snapshot still
        // contains this difficulty-one solution. Use the batch's difficulty.
        driver.hardware_difficulty = HARDWARE_DIFFICULTY;
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::quiet();

        let caller = async {
            // A fast work tick dispatches the published assignment into the
            // registry under work_id 0 well before the poll ticker, which
            // runs at the driver's period, gets a chance to fire.
            let start_result = actor.start_sending_work(Duration::from_millis(1)).await;
            assert!(matches!(start_result, Ok(())));
            let solution = SESSION.pool_handle(0).take_solution().await;
            let terminate_result = actor.terminate().await;
            assert!(matches!(terminate_result, Ok(())));
            solution
        };

        let (solution, run_result) = block_on(join(
            caller,
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(run_result, Ok(())));
        assert_eq!(solution.nonce, GENESIS_SOLUTION.nonce);
        assert_eq!(
            INGEST_CALLS.load(Ordering::Relaxed),
            0,
            "the poll path must never call ingest"
        );
    });
}

#[test]
fn run_clamps_a_driver_reporting_more_work_slots_than_the_registry_holds() {
    // See the comment on
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);
        static LAST_WORK_ID: AtomicU32 = AtomicU32::new(0);
        static LAST_NTIME: AtomicU32 = AtomicU32::new(0);
        static SEND_CALLS: AtomicU32 = AtomicU32::new(0);

        let registry_slots =
            u32::try_from(WORK_REGISTRY_SLOTS).expect("registry size fits a u32 in any test build");
        let numbered = |n: u32| WorkAssignment {
            ntime: n,
            ..assignment(n)
        };
        // Two assignments past a full lap, so the last work ID is on the
        // second lap.
        let last = registry_slots + 2;
        SESSION.pool_handle(0).publish_work(numbered(1));

        let actor = Serial::new(HashboardIndex(0), 1);
        let driver = ResultDriver::new(ResultMode::Push, &LINE_ERRORS, &INGEST_CALLS)
            .with_work_slots(WORK_REGISTRY_SLOTS + 8)
            .with_send_work_tracking(SendWorkTracking {
                last_work_id: &LAST_WORK_ID,
                last_ntime: &LAST_NTIME,
                calls: &SEND_CALLS,
            });
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::quiet();

        let caller = async {
            let start_result = actor.start_sending_work(Duration::from_millis(1)).await;
            assert!(matches!(start_result, Ok(())));
            // Publish assignment `n` only after a send was observed since
            // the previous publish. Every send after a publish carries that
            // assignment, so each one goes out at least once and in order,
            // whatever the tick timing.
            for n in 2..=last {
                let sent = SEND_CALLS.load(Ordering::Relaxed);
                while SEND_CALLS.load(Ordering::Relaxed) == sent {
                    yield_now().await;
                }
                SESSION.pool_handle(0).publish_work(numbered(n));
            }
            while LAST_NTIME.load(Ordering::Relaxed) != last {
                yield_now().await;
            }
            let terminate_result = actor.terminate().await;
            assert!(matches!(terminate_result, Ok(())));
        };

        let ((), run_result) = block_on(join(
            caller,
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(run_result, Ok(())));
        assert_eq!(LAST_NTIME.load(Ordering::Relaxed), last);
        assert_eq!(
            LAST_WORK_ID.load(Ordering::Relaxed),
            (last - 1) % registry_slots,
            "work IDs must wrap at the registry size, not the driver's claimed slot count"
        );
    });
}

/// Scheduler stub that only records the weights `set_weights` last
/// received, into `'static` counters so a test can read them after
/// `run` consumes and drops the scheduler. `evaluate` never offers a
/// slot and `accumulate_share` is a no-op: this stub isolates the
/// `SetPoolWeights` interception from work dispatch.
struct WeightsRecorder {
    recorded: &'static [AtomicU32; 8],
}

impl WeightsRecorder {
    fn new(recorded: &'static [AtomicU32; 8]) -> Self {
        Self { recorded }
    }
}

impl<const N: usize> Scheduler<N> for WeightsRecorder {
    fn accumulate_share(&mut self, _slot: usize) {}

    fn evaluate(&self, _available: &[bool; N]) -> Option<usize> {
        None
    }

    fn set_weights(&mut self, weights: &[u32]) {
        for (slot, dest) in self.recorded.iter().enumerate() {
            dest.store(weights.get(slot).copied().unwrap_or(0), Ordering::Relaxed);
        }
    }
}

#[test]
fn published_pool_weights_reach_the_scheduler() {
    // See the comment on
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);
        static RECORDED: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];

        let actor = Serial::new(HashboardIndex(0), 1);
        let driver = ResultDriver::new(ResultMode::Push, &LINE_ERRORS, &INGEST_CALLS);
        let scheduler = WeightsRecorder::new(&RECORDED);
        let serial = ScriptedSerial::quiet();
        let weights = [5, 0, 3, 0, 0, 0, 0, 1];

        let caller = async {
            // Fire-and-forget and synchronous: the publish takes no lock
            // and waits for nothing, so the terminate that follows is
            // what gives the actor the pass through its loop that
            // applies the weights.
            actor.set_pool_weights(weights);
            let terminate_result = actor.terminate().await;
            assert!(matches!(terminate_result, Ok(())));
        };

        let ((), run_result) = block_on(join(
            caller,
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(run_result, Ok(())));
        for (slot, &expected) in weights.iter().enumerate() {
            assert_eq!(
                RECORDED[slot].load(Ordering::Relaxed),
                expected,
                "the actor must apply the published weights to the scheduler"
            );
        }
    });
}

#[test]
fn a_command_waits_for_an_in_flight_poll_and_the_solution_survives() {
    // See the comment on
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        static SESSION: AsicSession<1> = AsicSession::new();
        static LINE_ERRORS: AtomicU32 = AtomicU32::new(0);
        static INGEST_CALLS: AtomicU32 = AtomicU32::new(0);
        static POLL_ENTERED: PollGate = Signal::new();
        static POLL_GATE: PollGate = Signal::new();

        // The pool target is the diff-1 difficulty itself, so the genesis header
        // GENESIS_SOLUTION solves also clears the pool target and the
        // solution reaches the session's solution channel, proving it was
        // not merely decoded but actually delivered.
        let assignment = WorkAssignment {
            target: *ShareTarget::DIFF_1,
            ..genesis_assignment()
        };
        SESSION.pool_handle(0).publish_work(assignment);

        let actor = Serial::new(HashboardIndex(0), 1);
        let driver = ResultDriver::new(POLL, &LINE_ERRORS, &INGEST_CALLS)
            .with_poll_solution(GENESIS_SOLUTION)
            .with_poll_rendezvous(&POLL_ENTERED, &POLL_GATE);
        let scheduler = Pinned::new(0);
        let serial = ScriptedSerial::quiet();

        let caller = async {
            let start_result = actor.start_sending_work(Duration::from_millis(1)).await;
            assert!(matches!(start_result, Ok(())));

            // Wait until a poll tick has entered `poll_results` and parked on
            // the gate, so the command below races a poll that is genuinely
            // in flight rather than one that has not started yet.
            POLL_ENTERED.wait().await;

            // Leave the chunk pending across several work ticks. The command
            // cannot complete until the gate releases the original chunk.
            let command = actor.set_frequency(Frequency::from_mhz(400));
            let release = async {
                Timer::after(Duration::from_millis(20)).await;
                POLL_GATE.signal(());
            };
            let (freq_result, ()) = join(command, release).await;
            assert!(matches!(freq_result, Ok(())));

            let solution = SESSION.pool_handle(0).take_solution().await;
            let terminate_result = actor.terminate().await;
            assert!(matches!(terminate_result, Ok(())));
            solution
        };

        let (solution, run_result) = block_on(join(
            caller,
            actor.run_with(
                crate::fixture::park(),
                serial,
                &SESSION,
                &BMM101_HASHBOARD,
                driver,
                scheduler,
                &TEST_STATS,
            ),
        ));

        assert!(matches!(run_result, Ok(())));
        assert_eq!(
            solution.nonce, GENESIS_SOLUTION.nonce,
            "the solution collected before the wait still reaches the session"
        );
    });
}

/// Serial mock for driving [`SerialIo::read`] directly: the first read
/// pops a queued line error, the second delivers bytes.
struct FlakySerial {
    reads: u8,
}

impl embedded_io::ErrorType for FlakySerial {
    type Error = ErrorKind;
}

impl embedded_io_async::Write for FlakySerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_io_async::Read for FlakySerial {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.reads += 1;
        if self.reads == 1 {
            return Err(ErrorKind::Other);
        }
        buf[..3].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        Ok(3)
    }
}

impl BaudRate for FlakySerial {
    fn set_baudrate(&mut self, _baud: u32) -> Result<(), ErrorKind> {
        Ok(())
    }
}

#[test]
fn transport_read_consumes_a_line_error_and_returns_the_bytes_behind_it() {
    let mut serial = FlakySerial { reads: 0 };
    let mut io = SerialIo {
        serial: &mut serial,
    };
    let mut buf = [0u8; 8];

    let n = block_on(io.read(&mut buf, 50));

    assert_eq!(n, 3, "the read must survive the popped line error");
    assert_eq!(buf[..3], [0xAA, 0xBB, 0xCC]);
    assert_eq!(
        serial.reads, 2,
        "the error consumed one attempt, the bytes came on the next"
    );
}

#[test]
fn park_if_requested_parks_and_unparks_across_release() {
    use core::future::Future;

    // The park is this test's own, so its counts are exact. The critical
    // section is for the global logger; see
    // `read_of_zero_bytes_ends_the_run_and_the_same_actor_serves_a_later_command`.
    critical_section::with(|_| {
        let actor = Serial::new(HashboardIndex(0), 1);
        let mut serial = MockSerial::new();
        let waker = core::task::Waker::noop();
        let mut cx = core::task::Context::from_waker(waker);

        let park = crate::fixture::park();

        // Nothing requested yet: the future resolves immediately to
        // `false` on its very first poll.
        {
            let mut fut = core::pin::pin!(actor.park_if_requested(park, &mut serial));
            match fut.as_mut().poll(&mut cx) {
                core::task::Poll::Ready(parked) => assert!(!parked, "no park was requested"),
                core::task::Poll::Pending => {
                    panic!("an unrequested park must resolve immediately")
                }
            }
        }

        park.request();
        let _wake = park.join();

        let mut fut = core::pin::pin!(actor.park_if_requested(park, &mut serial));
        for _ in 0..3 {
            assert_eq!(
                fut.as_mut().poll(&mut cx),
                core::task::Poll::Pending,
                "the actor stays parked while a park is outstanding"
            );
            assert_eq!(park.parked_count(), 1, "exactly one actor is parked");
            assert!(park.is_parked());
        }

        park.release();

        match fut.as_mut().poll(&mut cx) {
            core::task::Poll::Ready(parked) => assert!(parked, "the release must unpark it"),
            core::task::Poll::Pending => panic!("a released park must resolve"),
        }
        assert_eq!(
            park.parked_count(),
            0,
            "unparking drops the parked count back to zero"
        );

        park.leave(false);
    });
}

#[test]
fn one_park_request_wakes_every_running_actor() {
    use core::future::Future;

    // See the comment on `park_if_requested_parks_and_unparks_across_release`.
    critical_section::with(|_| {
        let first = Serial::new(HashboardIndex(0), 2);
        let second = Serial::new(HashboardIndex(1), 2);
        let mut first_serial = MockSerial::new();
        let mut second_serial = MockSerial::new();
        let waker = core::task::Waker::noop();
        let mut cx = core::task::Context::from_waker(waker);

        let park = crate::fixture::park();
        let mut first_wake = park.join();
        let mut second_wake = park.join();
        // Drop whatever an earlier park left in the watch, so what the
        // assertions below see is this test's own request.
        let _ = first_wake.try_changed();
        let _ = second_wake.try_changed();

        park.request();

        assert!(
            first_wake.try_changed().is_some(),
            "the first actor must be woken by the request"
        );
        assert!(
            second_wake.try_changed().is_some(),
            "the second actor must be woken by the same request"
        );

        let mut first_park = core::pin::pin!(first.park_if_requested(park, &mut first_serial));
        let mut second_park = core::pin::pin!(second.park_if_requested(park, &mut second_serial));
        assert_eq!(first_park.as_mut().poll(&mut cx), core::task::Poll::Pending);
        assert_eq!(
            second_park.as_mut().poll(&mut cx),
            core::task::Poll::Pending
        );
        assert_eq!(
            park.parked_count(),
            2,
            "both actors park against the one request"
        );
        assert!(park.is_parked(), "every live actor is parked");

        park.release();

        assert_eq!(
            first_park.as_mut().poll(&mut cx),
            core::task::Poll::Ready(true)
        );
        assert_eq!(
            second_park.as_mut().poll(&mut cx),
            core::task::Poll::Ready(true)
        );
        assert_eq!(park.parked_count(), 0);

        park.leave(false);
        park.leave(false);
    });
}

/// A `WorkAssignment` whose pool grant is the full BIP320 mask and whose
/// target accepts any hash, so both chains' rebuilt headers reach the
/// submit path and the test reads what each chain rolled rather than
/// how hard it hashed.
fn granted_assignment() -> WorkAssignment {
    WorkAssignment {
        target: [0xFF; 32],
        version_mask: BIP320_VERSION_MASK,
        ..genesis_assignment()
    }
}

/// Dispatch one work tick for `chain` of `chains` against `session`, and
/// report the job it built together with the solution it submitted for a
/// chip echo of `version_bits`.
fn dispatch_and_submit(
    session: &AsicSession<1>,
    chain: HashboardIndex,
    chains: usize,
    version_bits: u32,
) -> (WorkJob, Solution) {
    let actor = Serial::new(chain, chains);
    let mut serial = MockSerial::new();
    let mut driver = StubDriver::new(ResultMode::Push, 0);
    let mut scheduler = Pinned::new(0);
    let mut registry = WorkRegistry::new(driver.work_slots());
    let mut receivers: [DynReceiver<'_, WorkAssignment>; 1] = core::array::from_fn(|slot| {
        session
            .work_receiver(slot)
            .expect("test session has a free receiver per slot")
    });
    let mut last_version_mask = None;

    block_on(actor.dispatch_work(
        &mut serial,
        &scheduler,
        &mut driver,
        &mut registry,
        &mut receivers,
        &mut last_version_mask,
    ));
    let job = driver.last_job.take().expect("the tick dispatched a job");

    let solution = ChainSolution {
        version_bits,
        ..GENESIS_SOLUTION
    };
    let stats = ChainStats::new();
    Serial::process_solution(
        &solution,
        &registry,
        session,
        &mut scheduler,
        &[0xFF; 32],
        &stats,
    );
    let submitted = session
        .pool_handle(0)
        .try_take_solution()
        .expect("a solution over a target that accepts every hash is submitted");
    (job, submitted)
}

#[test]
fn two_chains_hash_different_versions_and_each_rebuilds_its_own_solution() {
    // The chip echo: a rolled index inside both chains' rolled mask.
    const ECHO: u32 = 0x0000_2000;

    let session: AsicSession<1> = AsicSession::new();
    let assignment = granted_assignment();
    session.pool_handle(0).publish_work(assignment);

    let (first_job, first_submitted) = dispatch_and_submit(&session, HashboardIndex(0), 2, ECHO);
    session.pool_handle(0).publish_work(assignment);
    let (second_job, second_submitted) = dispatch_and_submit(&session, HashboardIndex(1), 2, ECHO);

    assert_ne!(
        first_job.version, second_job.version,
        "sibling chains must not hash the same header"
    );
    assert_eq!(
        first_job.version_mask, second_job.version_mask,
        "both chains roll the same bits; only the fixed bits differ"
    );
    assert_eq!(
        first_job.version_mask | (first_job.version ^ second_job.version),
        assignment.version_mask,
        "the rolled bits and the reserved index bits together are the whole grant"
    );

    for (job, submitted) in [
        (&first_job, &first_submitted),
        (&second_job, &second_submitted),
    ] {
        let expected = rolled_version(job.version, ECHO, job.version_mask);
        assert_eq!(
            submitted.version_bits,
            expected & assignment.version_mask,
            "each chain submits the header it hashed, under the pool's grant"
        );
    }
    assert_ne!(
        first_submitted.version_bits, second_submitted.version_bits,
        "the two chains' submissions carry their own index bits"
    );
}

#[test]
fn a_chain_with_nothing_to_roll_dispatches_no_work() {
    let session: AsicSession<1> = AsicSession::new();
    let assignment = WorkAssignment {
        target: [0xFF; 32],
        version_mask: 0,
        ..genesis_assignment()
    };

    for chain in 1..3 {
        session.pool_handle(0).publish_work(assignment);
        assert!(
            dispatch_job(&session, HashboardIndex(chain), 3).is_none(),
            "chain {chain} has no version bits of its own and must stay idle"
        );
    }

    session.pool_handle(0).publish_work(assignment);
    assert!(
        dispatch_job(&session, HashboardIndex(0), 3).is_some(),
        "chain 0 keeps the whole grant and goes on mining"
    );
}

/// Dispatch one work tick for `chain` of `chains` and report the job it
/// built, or `None` when the tick sent nothing.
fn dispatch_job(session: &AsicSession<1>, chain: HashboardIndex, chains: usize) -> Option<WorkJob> {
    let actor = Serial::new(chain, chains);
    let mut serial = MockSerial::new();
    let mut driver = StubDriver::new(ResultMode::Push, 0);
    let scheduler = Pinned::new(0);
    let mut registry = WorkRegistry::new(driver.work_slots());
    let mut receivers: [DynReceiver<'_, WorkAssignment>; 1] = core::array::from_fn(|slot| {
        session
            .work_receiver(slot)
            .expect("test session has a free receiver per slot")
    });
    let mut last_version_mask = None;

    block_on(actor.dispatch_work(
        &mut serial,
        &scheduler,
        &mut driver,
        &mut registry,
        &mut receivers,
        &mut last_version_mask,
    ));
    driver.last_job.take()
}

#[test]
fn table_numbers_each_actor_by_its_position() {
    static TABLE: [Serial; 3] = Serial::table();
    for (position, actor) in TABLE.iter().enumerate() {
        assert_eq!(usize::from(actor.chain().0), position);
        assert_eq!(actor.chains(), TABLE.len());
    }
}
