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

//! Coverage for `ChainHandle::bring_up` and `bring_up_all`: the presence
//! decisions they drive the enumeration loop to, the reset pulse
//! between retries, and the whole-machine `Done` / `NonePresent` split.
//!
//! Each test runs a real [`Serial`] actor concurrently with the
//! bring-up call, the same way `tests/serial.rs` drives `Serial::run`:
//! one future is the actor's server loop, the other is the bring-up
//! call itself, joined so both make progress. `BringUpDriver` scripts
//! the chip-count and configure outcomes `reset_and_enumerate` and
//! `configure` hand back; `LoggingResetPin` records the reset-pin call
//! sequence a chain's hardware reset produces.

use portable_atomic::AtomicU8;

use embassy_futures::block_on;
use embassy_futures::join::{join, join3};
use ubos_asic::{BMM101_HASHBOARD, ChainDriver, ChipDescriptor, ChipError, HashboardDescriptor};
use ubos_asic::{PollBatch, PollProgress, ResultMode, ShareTarget, SolutionBatch, Transport};
use ubos_core::units::Frequency;
use ubos_metrics::chain::{ChainPresence, ChainStats, ChainStatsHandle};

use crate::chain::ChainHandle;
use crate::reset::ResetPin;
use crate::scheduler::Scheduler;
use crate::serial::Serial;

use stratum_client::AsicSession;

use super::{BringUpOutcome, bring_up_all};
use crate::chain::presence::ENUMERATE_ATTEMPTS;

/// Reset-pin call recorded by [`LoggingResetPin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResetCall {
    Enter,
    Exit,
}

/// `ResetPin` mock that records every call in order, so a test can
/// assert the pulse sequence a retry drives.
struct LoggingResetPin {
    log: heapless::Vec<ResetCall, 16>,
}

impl LoggingResetPin {
    fn new() -> Self {
        Self {
            log: heapless::Vec::new(),
        }
    }
}

impl ResetPin for LoggingResetPin {
    async fn enter_reset(&mut self) {
        let _ = self.log.push(ResetCall::Enter);
    }

    async fn exit_reset(&mut self) {
        let _ = self.log.push(ResetCall::Exit);
    }
}

/// No-op scheduler for driving `Serial::run` through bring-up, which
/// never dispatches work.
struct NoWork;

impl Scheduler<1> for NoWork {
    fn accumulate_share(&mut self, _slot: usize) {}

    fn evaluate(&mut self, _available: &[bool; 1]) -> Option<usize> {
        None
    }
}

/// `ChainDriver` stub that hands `reset_and_enumerate` a scripted chip
/// count per call, in order, and holding the last one once the script
/// runs out. `configure` either succeeds or always refuses, per the
/// test.
struct BringUpDriver {
    counts: &'static [u8],
    call: usize,
    configure_err: bool,
}

impl BringUpDriver {
    fn new(counts: &'static [u8]) -> Self {
        Self {
            counts,
            call: 0,
            configure_err: false,
        }
    }

    fn with_configure_err(mut self) -> Self {
        self.configure_err = true;
        self
    }
}

impl ChainDriver for BringUpDriver {
    fn descriptor(&self) -> &'static ChipDescriptor {
        BMM101_HASHBOARD.chip
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
        let index = self.call.min(self.counts.len() - 1);
        self.call += 1;
        self.counts[index]
    }

    async fn configure(
        &mut self,
        _io: &mut impl Transport,
        _board: &HashboardDescriptor,
    ) -> Result<(), ChipError> {
        if self.configure_err {
            Err(ChipError::BaudSwitch(3_000_000))
        } else {
            Ok(())
        }
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
        _job: &ubos_asic::WorkJob,
    ) -> Result<(), ChipError> {
        Ok(())
    }

    fn result_mode(&self) -> ResultMode {
        ResultMode::Push
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
            difficulty: ShareTarget::DIFF_1,
            progress: PollProgress::Complete,
        }
    }

    fn reset(&mut self) {}
}

/// Serial mock: writes succeed, baud switches are no-ops, and reads
/// pend forever so only the actor's command arm can resolve its
/// select. Identical in shape to `tests/serial.rs`'s `ScriptedSerial`
/// with no step scripted, kept local since that type is private to
/// `serial`'s own test module.
struct QuietSerial;

impl embedded_io::ErrorType for QuietSerial {
    type Error = embedded_io::ErrorKind;
}

impl embedded_io_async::Read for QuietSerial {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        core::future::pending().await
    }
}

impl embedded_io_async::Write for QuietSerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serial_baudrate::BaudRate for QuietSerial {
    fn set_baudrate(&mut self, _baud: u32) -> Result<(), embedded_io::ErrorKind> {
        Ok(())
    }
}

/// One chain's worth of test fixture: a `Serial` actor backed by
/// `BringUpDriver`, its `ChainHandle` wired to a `LoggingResetPin` and
/// a caller-owned `ChainStats` slot, and the `AsicSession` `run`
/// needs.
struct ChainFixture {
    actor: &'static Serial,
    session: &'static AsicSession<1>,
    stats: &'static ChainStats,
}

/// Run `ChainHandle::bring_up` against `fixture`'s actor, driving the actor's
/// server loop concurrently, and return the resulting `ChainHandle` for
/// the test to inspect.
///
/// Runs inside `critical_section::with`, the same serialisation
/// `tests/serial.rs` uses around every test that drives `Serial::run`:
/// `run` counts itself into the module-global `LIVE_ACTORS` for the
/// park handshake and, under the `defmt` feature, writes through one
/// global logger, so two runs live at once would make either
/// meaningless.
fn run_bring_up_chain(
    fixture: &ChainFixture,
    driver: BringUpDriver,
    expected: u8,
) -> ChainHandle<LoggingResetPin> {
    critical_section::with(|_| {
        let mut chain = ChainHandle {
            serial: fixture.actor,
            stats: fixture.stats,
            reset: LoggingResetPin::new(),
        };
        let bring_up = async {
            chain.bring_up(expected).await;
            let terminate_result = fixture.actor.terminate().await;
            assert!(matches!(terminate_result, Ok(())));
        };
        let run = fixture.actor.run(
            QuietSerial,
            fixture.session,
            &BMM101_HASHBOARD,
            driver,
            NoWork,
            fixture.stats,
        );
        let ((), run_result) = block_on(join(bring_up, run));
        assert!(matches!(run_result, Ok(())));
        chain
    })
}

#[test]
fn a_chain_answering_the_expected_count_becomes_present_and_stays_out_of_reset() {
    static ACTOR: Serial = Serial::new(0, 1);
    static SESSION: AsicSession<1> = AsicSession::new();
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    const EXPECTED: u8 = 4;
    let fixture = ChainFixture {
        actor: &ACTOR,
        session: &SESSION,
        stats: ChainStatsHandle::new(&STATS, &ACTIVE).chain(0),
    };
    let driver = BringUpDriver::new(&[EXPECTED]);

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    assert_eq!(
        fixture.stats.presence(),
        ChainPresence::Present { chips: EXPECTED }
    );
    assert!(
        chain.reset.log.is_empty(),
        "a present chain is left as bring-up found it, with no reset pulse"
    );
}

#[test]
fn a_chain_answering_zero_chips_is_absent_on_the_first_attempt_with_no_retry_and_left_in_reset() {
    static ACTOR: Serial = Serial::new(0, 1);
    static SESSION: AsicSession<1> = AsicSession::new();
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    const EXPECTED: u8 = 4;
    let fixture = ChainFixture {
        actor: &ACTOR,
        session: &SESSION,
        stats: ChainStatsHandle::new(&STATS, &ACTIVE).chain(0),
    };
    let driver = BringUpDriver::new(&[0]);

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    assert_eq!(fixture.stats.presence(), ChainPresence::Absent);
    assert_eq!(
        chain.reset.log.as_slice(),
        &[ResetCall::Enter],
        "an absent chain is parked in reset with no retry pulse first"
    );
}

#[test]
fn a_wrong_non_zero_count_is_retried_up_to_the_attempt_budget_then_faulted_and_left_in_reset() {
    static ACTOR: Serial = Serial::new(0, 1);
    static SESSION: AsicSession<1> = AsicSession::new();
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    const EXPECTED: u8 = 4;
    const WRONG: u8 = 2;
    let fixture = ChainFixture {
        actor: &ACTOR,
        session: &SESSION,
        stats: ChainStatsHandle::new(&STATS, &ACTIVE).chain(0),
    };
    let driver = BringUpDriver::new(&[WRONG]);

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    assert_eq!(
        fixture.stats.presence(),
        ChainPresence::Faulted { found: WRONG }
    );
    // Every attempt but the last is followed by a reset pulse, and the
    // final Faulted verdict adds one more Enter to park the chain.
    let expected_pulses = usize::from(ENUMERATE_ATTEMPTS - 1) * 2 + 1;
    assert_eq!(chain.reset.log.len(), expected_pulses);
    assert_eq!(
        *chain
            .reset
            .log
            .last()
            .expect("bring_up always logs at least one reset call"),
        ResetCall::Enter,
        "the faulted chain is left in reset"
    );
}

#[test]
fn a_refused_configure_spends_the_same_budget_and_ends_faulted_with_the_expected_count() {
    static ACTOR: Serial = Serial::new(0, 1);
    static SESSION: AsicSession<1> = AsicSession::new();
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    const EXPECTED: u8 = 4;
    let fixture = ChainFixture {
        actor: &ACTOR,
        session: &SESSION,
        stats: ChainStatsHandle::new(&STATS, &ACTIVE).chain(0),
    };
    let driver = BringUpDriver::new(&[EXPECTED]).with_configure_err();

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    assert_eq!(
        fixture.stats.presence(),
        ChainPresence::Faulted { found: EXPECTED }
    );
    assert_eq!(
        *chain
            .reset
            .log
            .last()
            .expect("bring_up always logs at least one reset call"),
        ResetCall::Enter,
        "the faulted chain is left in reset"
    );
}

#[test]
fn between_two_attempts_the_chains_reset_is_pulsed_enter_then_exit() {
    static ACTOR: Serial = Serial::new(0, 1);
    static SESSION: AsicSession<1> = AsicSession::new();
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    const EXPECTED: u8 = 4;
    const WRONG: u8 = 2;
    let fixture = ChainFixture {
        actor: &ACTOR,
        session: &SESSION,
        stats: ChainStatsHandle::new(&STATS, &ACTIVE).chain(0),
    };
    let driver = BringUpDriver::new(&[WRONG]);

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    // The retry pulse between the two attempts is Enter followed by
    // Exit, at the head of the log.
    assert_eq!(
        &chain.reset.log[..2],
        &[ResetCall::Enter, ResetCall::Exit],
        "the retry pulse enters and then exits reset"
    );
}

#[test]
fn bring_up_all_returns_none_present_when_no_chain_comes_up() {
    // See the doc comment on `run_bring_up_chain`: the two actors
    // driven here must not overlap another test's `Serial::run`.
    critical_section::with(|_| {
        static ACTOR_A: Serial = Serial::new(0, 2);
        static ACTOR_B: Serial = Serial::new(1, 2);
        static SESSION: AsicSession<1, 2> = AsicSession::new();
        static STATS: [ChainStats; 2] = [ChainStats::new(), ChainStats::new()];
        static ACTIVE: AtomicU8 = AtomicU8::new(2);

        const EXPECTED: u8 = 4;
        let stats_handle = ChainStatsHandle::new(&STATS, &ACTIVE);
        let mut chains = [
            ChainHandle {
                serial: &ACTOR_A,
                stats: stats_handle.chain(0),
                reset: LoggingResetPin::new(),
            },
            ChainHandle {
                serial: &ACTOR_B,
                stats: stats_handle.chain(1),
                reset: LoggingResetPin::new(),
            },
        ];

        let bring_up = async {
            let bring_up_outcome = bring_up_all(&mut chains, EXPECTED).await;
            let terminate_a = ACTOR_A.terminate().await;
            let terminate_b = ACTOR_B.terminate().await;
            assert!(matches!(terminate_a, Ok(())));
            assert!(matches!(terminate_b, Ok(())));
            bring_up_outcome
        };
        let run_a = ACTOR_A.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            BringUpDriver::new(&[0]),
            NoWork,
            stats_handle.chain(0),
        );
        let run_b = ACTOR_B.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            BringUpDriver::new(&[0]),
            NoWork,
            stats_handle.chain(1),
        );

        let (bring_up_outcome, primary_run, secondary_run) =
            block_on(join3(bring_up, run_a, run_b));
        assert!(matches!(primary_run, Ok(())));
        assert!(matches!(secondary_run, Ok(())));
        assert!(matches!(bring_up_outcome, BringUpOutcome::NonePresent));
    });
}

#[test]
fn bring_up_all_returns_done_when_at_least_one_chain_comes_up_present() {
    // See the doc comment on `run_bring_up_chain`.
    critical_section::with(|_| {
        static ACTOR_A: Serial = Serial::new(0, 2);
        static ACTOR_B: Serial = Serial::new(1, 2);
        static SESSION: AsicSession<1, 2> = AsicSession::new();
        static STATS: [ChainStats; 2] = [ChainStats::new(), ChainStats::new()];
        static ACTIVE: AtomicU8 = AtomicU8::new(2);

        const EXPECTED: u8 = 4;
        let stats_handle = ChainStatsHandle::new(&STATS, &ACTIVE);
        let mut chains = [
            ChainHandle {
                serial: &ACTOR_A,
                stats: stats_handle.chain(0),
                reset: LoggingResetPin::new(),
            },
            ChainHandle {
                serial: &ACTOR_B,
                stats: stats_handle.chain(1),
                reset: LoggingResetPin::new(),
            },
        ];

        let bring_up = async {
            let bring_up_outcome = bring_up_all(&mut chains, EXPECTED).await;
            let terminate_a = ACTOR_A.terminate().await;
            let terminate_b = ACTOR_B.terminate().await;
            assert!(matches!(terminate_a, Ok(())));
            assert!(matches!(terminate_b, Ok(())));
            bring_up_outcome
        };
        // Chain A comes up present; chain B stays absent.
        let run_a = ACTOR_A.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            BringUpDriver::new(&[EXPECTED]),
            NoWork,
            stats_handle.chain(0),
        );
        let run_b = ACTOR_B.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            BringUpDriver::new(&[0]),
            NoWork,
            stats_handle.chain(1),
        );

        let (bring_up_outcome, primary_run, secondary_run) =
            block_on(join3(bring_up, run_a, run_b));
        assert!(matches!(primary_run, Ok(())));
        assert!(matches!(secondary_run, Ok(())));
        assert!(matches!(bring_up_outcome, BringUpOutcome::Done));
        assert_eq!(
            stats_handle.chain(0).presence(),
            ChainPresence::Present { chips: EXPECTED }
        );
        assert_eq!(stats_handle.chain(1).presence(), ChainPresence::Absent);
    });
}
