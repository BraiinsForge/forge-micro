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

//! Coverage for `ChainHandle::bring_up` and the miner's `BringUp` state: the presence
//! decisions they drive the enumeration loop to, the reset pulse
//! between retries, and the whole-machine `StartWork` / `Recover` split.
//!
//! Each test runs a real [`Serial`] actor concurrently with the
//! bring-up call, the same way `tests/serial.rs` drives `Serial::run`:
//! one future is the actor's server loop, the other is the bring-up
//! call itself, joined so both make progress. `BringUpDriver` scripts
//! the chip-count and configure outcomes `reset_and_enumerate` and
//! `configure` hand back; `LoggingResetPin` records the reset-pin call
//! sequence a chain's hardware reset produces.

use super::Miner;
use super::machine::MinerState;
use super::states::BringUp;

#[path = "machine.rs"]
mod machine;
use crate::control::{COMMAND_CHANNEL, Message};
use crate::tuner::MachineTuner;
use core::cell::{Cell, RefCell};
use core::convert::Infallible;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_time::Instant;
use embassy_time::{Duration, Timer};
use portable_atomic::{AtomicU8, Ordering};
use ubos_config::TunerInfo;
use ubos_core::units::{Power, Voltage};
use ubos_psu::{Error as PsuError, Psu, StatusFlags};
use ubos_tuner::{Plan, Readings};

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

    fn evaluate(&self, _available: &[bool; 1]) -> Option<usize> {
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
    silent_sweep: bool,
    plan_log: Option<(usize, &'static HardwareLog)>,
    refused_frequency: Option<Frequency>,
    stop_during_frequency: Option<Duration>,
}

impl BringUpDriver {
    fn new(counts: &'static [u8]) -> Self {
        Self {
            counts,
            call: 0,
            configure_err: false,
            silent_sweep: false,
            plan_log: None,
            refused_frequency: None,
            stop_during_frequency: None,
        }
    }

    fn with_configure_err(mut self) -> Self {
        self.configure_err = true;
        self
    }

    /// A driver whose chips enumerate and configure but answer no PVT
    /// read, which is what a chain with a dead sensor path looks like.
    fn with_silent_sweep(mut self) -> Self {
        self.silent_sweep = true;
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
        board: &HashboardDescriptor,
    ) -> Result<Frequency, ChipError> {
        if self.configure_err {
            Err(ChipError::BaudSwitch(3_000_000))
        } else {
            Ok(board.chip.frequency_range.min())
        }
    }

    async fn pvt_sweep(&mut self, _io: &mut impl Transport) -> bool {
        !self.silent_sweep
    }

    async fn set_frequency(
        &mut self,
        _io: &mut impl Transport,
        freq: Frequency,
    ) -> Result<(), ChipError> {
        if let Some((chain, log)) = self.plan_log {
            log.lock(|log| {
                log.borrow_mut()
                    .push(Write::Frequency(chain, freq))
                    .expect("write log capacity");
            });
        }
        if let Some(delay) = self.stop_during_frequency.take() {
            crate::handle().stop();
            Timer::after(delay).await;
        }
        if self.refused_frequency == Some(freq) {
            return Err(ChipError::UnsupportedFrequency(freq));
        }
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
fn a_chain_answering_zero_chips_is_retried_to_the_budget_then_absent_and_left_in_reset() {
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
        &[
            ResetCall::Enter,
            ResetCall::Exit,
            ResetCall::Enter,
            ResetCall::Exit,
            ResetCall::Enter
        ],
        "a silent chain is pulsed through reset between attempts and parked in reset"
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
fn a_chain_whose_pvt_sweep_reads_nothing_is_faulted_rather_than_present() {
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
    // The chain enumerates its full count and takes its configuration;
    // only the temperature read answers nothing.
    let driver = BringUpDriver::new(&[EXPECTED]).with_silent_sweep();

    let chain = run_bring_up_chain(&fixture, driver, EXPECTED);

    assert_eq!(
        fixture.stats.presence(),
        ChainPresence::Faulted { found: EXPECTED },
        "a chain with no reading behind it must not reach the ramp"
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
fn a_bring_up_with_no_chain_present_recovers() {
    // See the doc comment on `run_bring_up_chain`: the two actors
    // driven here must not overlap another test's `Serial::run`.
    critical_section::with(|_| {
        static ACTOR_A: Serial = Serial::new(0, 2);
        static ACTOR_B: Serial = Serial::new(1, 2);
        static SESSION: AsicSession<1, 2> = AsicSession::new();
        static STATS: [ChainStats; 2] = [ChainStats::new(), ChainStats::new()];
        static ACTIVE: AtomicU8 = AtomicU8::new(2);
        static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
        let psu = TestPsu {
            log: &LOG,
            disable_fails: false,
            behavior: Cell::new(VoltageWrite::Succeed),
        };
        let stats_handle = ChainStatsHandle::new(&STATS, &ACTIVE);
        let mut miner = test_miner::<crate::tuner::Tuners, 2>(
            &psu,
            [&ACTOR_A, &ACTOR_B],
            [stats_handle.chain(0), stats_handle.chain(1)],
        );
        let mut subscription = test_config().0;

        let bring_up = async {
            let bring_up_outcome = miner
                .advance(MinerState::BringUp(BringUp), &mut subscription)
                .await;
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
        assert!(matches!(bring_up_outcome, Some(MinerState::Recover(_))));
    });
}

/// The outer budget: every cycle that brings no chain up cuts the rail
/// and tries again, until `BRING_UP_CYCLES` is spent and the machine
/// parks instead. A cycle that does bring a chain up starts the count
/// again, so an occasional empty cycle never reaches the park.
#[test]
fn empty_cycles_spend_the_outer_budget_and_a_good_cycle_starts_it_again() {
    static ACTOR: Serial = Serial::new(0, 1);
    static STATS: [ChainStats; 1] = [ChainStats::new()];
    static ACTIVE: AtomicU8 = AtomicU8::new(1);

    static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
    let psu = TestPsu {
        log: &LOG,
        disable_fails: false,
        behavior: Cell::new(VoltageWrite::Succeed),
    };
    let stats_handle = ChainStatsHandle::new(&STATS, &ACTIVE);
    let mut miner = test_miner::<crate::tuner::Tuners, 1>(&psu, [&ACTOR], [stats_handle.chain(0)]);

    for cycle in 1..super::BRING_UP_CYCLES {
        assert!(
            !miner.empty_cycle_spends_the_budget(),
            "cycle {cycle} of {} must still cut the rail and retry",
            super::BRING_UP_CYCLES
        );
    }
    assert!(
        miner.empty_cycle_spends_the_budget(),
        "the last cycle of the budget parks the machine"
    );

    miner.empty_cycles = 0;
    assert!(
        !miner.empty_cycle_spends_the_budget(),
        "a cycle that brought a chain up gives the budget back"
    );
}

#[test]
fn a_bring_up_with_one_chain_present_starts_work() {
    // See the doc comment on `run_bring_up_chain`.
    critical_section::with(|_| {
        static ACTOR_A: Serial = Serial::new(0, 2);
        static ACTOR_B: Serial = Serial::new(1, 2);
        static SESSION: AsicSession<1, 2> = AsicSession::new();
        static STATS: [ChainStats; 2] = [ChainStats::new(), ChainStats::new()];
        static ACTIVE: AtomicU8 = AtomicU8::new(2);
        static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
        const EXPECTED: u8 = BMM101_HASHBOARD.num_chips;
        let psu = TestPsu {
            log: &LOG,
            disable_fails: false,
            behavior: Cell::new(VoltageWrite::Succeed),
        };
        let stats_handle = ChainStatsHandle::new(&STATS, &ACTIVE);
        let mut miner = test_miner::<crate::tuner::Tuners, 2>(
            &psu,
            [&ACTOR_A, &ACTOR_B],
            [stats_handle.chain(0), stats_handle.chain(1)],
        );
        let mut subscription = test_config().0;

        let bring_up = async {
            let bring_up_outcome = miner
                .advance(MinerState::BringUp(BringUp), &mut subscription)
                .await;
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
        assert!(matches!(bring_up_outcome, Some(MinerState::StartWork(_))));
        assert_eq!(
            stats_handle.chain(0).presence(),
            ChainPresence::Present { chips: EXPECTED }
        );
        assert_eq!(stats_handle.chain(1).presence(), ChainPresence::Absent);
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Write {
    Frequency(usize, Frequency),
    Voltage(Voltage),
    Enable,
    Disable,
}

type HardwareLog = Mutex<CriticalSectionRawMutex, RefCell<heapless::Vec<Write, 32>>>;

#[derive(Clone, Copy)]
enum VoltageWrite {
    Succeed,
    Fail,
    /// Each write takes the head of the script: `true` succeeds, `false`
    /// fails. Writes past the end succeed.
    Script(&'static [bool]),
    Interrupt,
    StopThenSucceed,
    YieldThenSucceed,
}

struct TestPsu {
    log: &'static HardwareLog,
    behavior: Cell<VoltageWrite>,
    disable_fails: bool,
}

impl Psu for TestPsu {
    type BusError = Infallible;
    async fn enable(&self) -> Result<(), PsuError<Infallible>> {
        self.log.lock(|log| {
            log.borrow_mut()
                .push(Write::Enable)
                .expect("write log capacity");
        });
        Ok(())
    }
    async fn disable(&self) -> Result<(), PsuError<Infallible>> {
        self.log.lock(|log| {
            log.borrow_mut()
                .push(Write::Disable)
                .expect("write log capacity");
        });
        if self.disable_fails {
            Err(PsuError::Timeout)
        } else {
            Ok(())
        }
    }
    async fn set_voltage(&self, voltage: Voltage) -> Result<(), PsuError<Infallible>> {
        self.log.lock(|log| {
            log.borrow_mut()
                .push(Write::Voltage(voltage))
                .expect("write log capacity");
        });
        match self.behavior.get() {
            VoltageWrite::Succeed => {}
            VoltageWrite::Fail => return Err(PsuError::OutOfRange),
            VoltageWrite::Script(script) => {
                let (outcome, rest) = script.split_first().unwrap_or((&true, &[]));
                self.behavior.set(VoltageWrite::Script(rest));
                if !outcome {
                    return Err(PsuError::OutOfRange);
                }
            }
            VoltageWrite::Interrupt => {
                self.behavior.set(VoltageWrite::Succeed);
                crate::handle().stop();
                core::future::pending::<()>().await;
            }
            VoltageWrite::YieldThenSucceed => {
                embassy_futures::yield_now().await;
            }
            VoltageWrite::StopThenSucceed => {
                self.behavior.set(VoltageWrite::Succeed);
                crate::handle().stop();
                embassy_futures::yield_now().await;
            }
        }
        ubos_psu::record_set_voltage(voltage);
        Ok(())
    }
    async fn read_input_power(&self) -> Result<Power, PsuError<Infallible>> {
        panic!("unexpected read")
    }
    async fn read_output_power(&self) -> Result<Power, PsuError<Infallible>> {
        panic!("unexpected read")
    }
    async fn read_output_voltage(&self) -> Result<Voltage, PsuError<Infallible>> {
        panic!("unexpected read")
    }
    async fn read_status(&self) -> Result<StatusFlags, PsuError<Infallible>> {
        panic!("unexpected read")
    }
    async fn terminate(&self) {
        panic!("unexpected termination")
    }
}

fn test_miner<'a, T: MachineTuner<CHAINS, PlatformReadings = ()>, const CHAINS: usize>(
    psu: &'a TestPsu,
    actors: [&'static Serial; CHAINS],
    stats: [&'static ChainStats; CHAINS],
) -> Miner<'a, TestPsu, LoggingResetPin, T, CHAINS> {
    Miner {
        psu,
        board: &BMM101_HASHBOARD,
        chains: core::array::from_fn(|index| ChainHandle {
            serial: actors[index],
            stats: stats[index],
            reset: LoggingResetPin::new(),
        }),
        read_platform: || (),
        mining: crate::handle(),
        empty_cycles: 0,
    }
}

fn test_config() -> (
    ubos_config::ConfigSubscription,
    impl Fn(ubos_config::Config),
) {
    extern crate std;
    let watch = &*std::boxed::Box::leak(std::boxed::Box::new(embassy_sync::watch::Watch::new()));
    watch.sender().send(ubos_config::Config::default());
    (
        watch.receiver().expect("configuration receiver"),
        move |config| watch.sender().send(config),
    )
}

#[test]
fn plans_order_shared_rail_writes_and_preserve_successful_partial_progress() {
    critical_section::with(|_| {
        static A: Serial = Serial::new(0, 3);
        static ABSENT: Serial = Serial::new(1, 3);
        static B: Serial = Serial::new(2, 3);
        static STATS: [ChainStats; 3] = [const { ChainStats::new() }; 3];
        static SESSION: AsicSession<1, 3> = AsicSession::new();
        static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
        let psu = TestPsu {
            log: &LOG,
            disable_fails: false,
            behavior: Cell::new(VoltageWrite::Succeed),
        };
        let mut miner = test_miner(&psu, [&A, &ABSENT, &B], STATS.each_ref());
        STATS[0].set_presence(ChainPresence::Present { chips: 4 });
        STATS[1].set_presence(ChainPresence::Absent);
        STATS[2].set_presence(ChainPresence::Present { chips: 4 });
        STATS[0].set_frequency(Frequency::from_mhz(600));
        STATS[2].set_frequency(Frequency::from_mhz(100));
        let refused = Frequency::from_mhz(300);
        let driver = |chain| BringUpDriver {
            plan_log: Some((chain, &LOG)),
            refused_frequency: Some(refused),
            ..BringUpDriver::new(&[4])
        };
        let exercise = async {
            // No actor runs for the absent connector: touching it would time out.
            miner.start_chains().await.expect("work starts");
            LOG.lock(|log| log.borrow_mut().clear());
            let lower = Frequency::from_mhz(500);
            let higher = Frequency::from_mhz(200);
            let voltage = Voltage::from_mv(1000);
            let plan = Plan {
                frequencies: [Some(lower), Some(refused), Some(higher)],
                rail: Some(voltage),
                delay: Duration::from_millis(0),
            };
            miner.apply_plan(&plan).await.expect("plan succeeds");
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[
                        Write::Frequency(0, lower),
                        Write::Voltage(voltage),
                        Write::Frequency(2, higher),
                    ]
                );
            });
            let readings = miner.readings();
            assert!(readings.chains[1].is_none());
            assert_eq!(readings.chains[0].expect("present chain").frequency, lower);
            assert_eq!(readings.chains[2].expect("present chain").frequency, higher);
            assert_eq!(readings.voltage, voltage);
            LOG.lock(|log| log.borrow_mut().clear());
            miner.apply_plan(&plan).await.expect("plan succeeds");
            LOG.lock(|log| assert!(log.borrow().is_empty()));

            check_voltage_only_plans(&mut miner, &LOG, &plan).await;

            check_partial_plan_failures(&mut miner, &LOG, refused, voltage, higher).await;
            check_plan_bounds(&mut miner, &LOG).await;
            A.terminate().await.expect("actor terminates");
            B.terminate().await.expect("actor terminates");
        };
        let run_a = A.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            driver(0),
            NoWork,
            &STATS[0],
        );
        let run_b = B.run(
            QuietSerial,
            &SESSION,
            &BMM101_HASHBOARD,
            driver(2),
            NoWork,
            &STATS[2],
        );
        let outcome = block_on(select(
            join3(exercise, run_a, run_b),
            Timer::after(Duration::from_millis(500)),
        ));
        let Either::First(((), a, b)) = outcome else {
            panic!("plan execution timed out")
        };
        a.expect("actor A succeeds");
        b.expect("actor B succeeds");
    });
}

async fn check_voltage_only_plans(
    miner: &mut Miner<'_, TestPsu, LoggingResetPin, crate::tuner::Tuners, 3>,
    log: &HardwareLog,
    plan: &Plan<3>,
) {
    // Unchanged or omitted clocks only update the shared voltage.
    for frequencies in [plan.frequencies, [None; 3]] {
        let changed_voltage = Voltage::from_mv(1050);
        miner
            .apply_plan(&Plan {
                frequencies,
                rail: Some(changed_voltage),
                ..*plan
            })
            .await
            .expect("voltage-only plan succeeds");
        log.lock(|log| {
            assert_eq!(log.borrow().as_slice(), &[Write::Voltage(changed_voltage)]);
            log.borrow_mut().clear();
        });
        miner.apply_plan(plan).await.expect("baseline restored");
        log.lock(|log| log.borrow_mut().clear());
    }
}

async fn check_partial_plan_failures(
    miner: &mut Miner<'_, TestPsu, LoggingResetPin, crate::tuner::Tuners, 3>,
    log: &HardwareLog,
    refused: Frequency,
    voltage: Voltage,
    higher: Frequency,
) {
    // A failed decrease prevents the rail change and every increase.
    let next_voltage = Voltage::from_mv(1100);
    let plan = Plan {
        frequencies: [Some(refused), None, Some(Frequency::from_mhz(250))],
        rail: Some(next_voltage),
        delay: Duration::from_millis(0),
    };
    assert!(miner.apply_plan(&plan).await.is_err());
    log.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Frequency(0, refused)]));
    assert_eq!(miner.readings().voltage, voltage);
    log.lock(|log| log.borrow_mut().clear());

    // A failed increase keeps the preceding successful rail change visible.
    let plan = Plan {
        frequencies: [None, None, Some(refused)],
        ..plan
    };
    assert!(miner.apply_plan(&plan).await.is_err());
    assert_eq!(miner.readings().voltage, next_voltage);
    assert_eq!(
        miner.readings().chains[2].expect("present chain").frequency,
        higher
    );
    log.lock(|log| {
        assert_eq!(
            log.borrow().as_slice(),
            &[Write::Voltage(next_voltage), Write::Frequency(2, refused),]
        );
    });
}

async fn check_plan_bounds(
    miner: &mut Miner<'_, TestPsu, LoggingResetPin, crate::tuner::Tuners, 3>,
    log: &HardwareLog,
) {
    let range = miner.board.chip.frequency_range;
    log.lock(|log| log.borrow_mut().clear());
    miner
        .apply_plan(&Plan {
            frequencies: [
                Some(Frequency::from_khz(0)),
                None,
                Some(Frequency::from_khz(u32::MAX)),
            ],
            rail: Some(Voltage::from_mv(u32::MAX)),
            delay: Duration::from_millis(0),
        })
        .await
        .expect("bounded plan succeeds");
    log.lock(|log| {
        assert_eq!(
            log.borrow().as_slice(),
            &[
                Write::Frequency(0, range.min()),
                Write::Voltage(miner.board.max_rail_voltage()),
                Write::Frequency(2, range.max()),
            ]
        );
    });
}
