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

//! Coverage for the state machine of the miner: each state's transition,
//! and the select that interrupts it.

use super::*;
use crate::miner::machine::{Context, INTERRUPT_TIMEOUT, MinerState};
use crate::miner::pool::{POOL_POLL_PERIOD, PoolWatch};
use crate::miner::states::{
    BringUp, Choose, Enable, ExitReset, Halted, PLAN_FAILURE_BUDGET, Pause, Recover, Reset, Run,
    Selected, StartWork, Step, WaitForPool,
};
use crate::scheduler::{FeeScheduler, RatioScheduler};
use rusty_fork::rusty_fork_test;
use ubos_config::DEVFEE;
use ubos_core::index::HashboardIndex;

/// The run of `tuner`, as [`StartWork`] records it for the default
/// configuration. Every tuner in this file serves that variant.
fn run<M>(tuner: M) -> Run<M> {
    Run {
        tuner,
        selected: fixed_selection(),
        failures: 0,
        pool: PoolWatch::default(),
    }
}

/// Run the machine from `state` until STOP ends it, which returns `None`,
/// or until it reaches [`Halted`], which is returned.
async fn drive<M: MachineTuner<CHAINS>, const CHAINS: usize>(
    miner: &mut Miner<LoggingResetPin, M, CHAINS>,
    mut state: MinerState<M>,
    subscription: &mut ubos_config::ConfigSubscription,
) -> Option<MinerState<M>> {
    loop {
        state = miner.advance(state, subscription).await?;
        if matches!(state, MinerState::Halted(_)) {
            return Some(state);
        }
    }
}

/// Drive the machine from [`Step`] with a real actor, so cancelled
/// writes can complete after the rail cut. Returns what [`drive`] returns.
fn run_mining_with_driver<M: MachineTuner<1>>(
    psu: &'static TestSupply,
    miner: &mut Miner<LoggingResetPin, M, 1>,
    tuner: M,
    subscription: &mut ubos_config::ConfigSubscription,
    driver: BringUpDriver,
) -> Option<MinerState<M>> {
    static SESSION: AsicSession<1, 1> = AsicSession::new();
    let actor = miner.chains[0].serial;
    let stats = miner.chains[0].stats;
    let exercise = async {
        let first = MinerState::Step(Step { run: run(tuner) });
        let outcome = drive(miner, first, subscription).await;
        actor.terminate().await.expect("actor terminates");
        outcome
    };
    let run = actor.run(
        crate::fixture::park(),
        QuietSerial,
        &SESSION,
        &BMM101_HASHBOARD,
        driver,
        NoWork,
        stats,
    );
    match block_on_with(
        psu,
        select(
            join(exercise, run),
            Timer::after(Duration::from_millis(500)),
        ),
    ) {
        Either::First((outcome, result)) => {
            result.expect("actor succeeds");
            outcome
        }
        Either::Second(()) => panic!("shutdown timed out"),
    }
}

struct OnePlanTuner(Plan<1>);

impl MachineTuner<1> for OnePlanTuner {
    type PlatformReadings = ();

    // The miner never reads the list.
    const TUNERS: &'static [TunerInfo] = &[];

    fn select(_: &ubos_config::Tuning, _: &'static HashboardDescriptor) -> Option<Self> {
        Some(Self(Plan {
            rail: None,
            frequencies: [None; 1],
            delay: Duration::from_secs(1),
        }))
    }

    fn step(&mut self, _: &ubos_config::Tuning, _readings: &Readings<1>, (): &()) -> Plan<1> {
        self.0
    }
}

rusty_fork_test! {
    #[test]
    fn stop_finishes_the_pending_voltage_write_without_a_shutdown_ramp() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::StopThenSucceed),
                disable_fails: false,
            });
            STATS.set_presence(ChainPresence::Present { chips: 4 });
            STATS.set_frequency(BMM101_HASHBOARD.chip.frequency_range.min());
            seed_voltage(psu_driver, psu, BMM101_HASHBOARD.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let voltage = Voltage::from_mv(1100);
            let tuner = OnePlanTuner(Plan {
                frequencies: [None],
                rail: Some(voltage),
                delay: Duration::from_millis(500),
            });
            let mut subscription = test_config().0;
            let outcome = run_mining_with_driver(
                psu,
                &mut miner,
                tuner,
                &mut subscription,
                BringUpDriver::new(&[4]),
            );
            assert!(outcome.is_none());
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Voltage(voltage), Write::Disable,]
                );
            });
            assert_eq!(psu.handle.commanded_voltage(), Some(voltage));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn cancellation_timeout_cuts_power_without_a_shutdown_frequency_write() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let safe = BMM101_HASHBOARD.chip.frequency_range.min();
            let raised = Frequency::from_khz(safe.khz() + BMM101_HASHBOARD.chip.frequency_step.khz());
            STATS.set_presence(ChainPresence::Present { chips: 4 });
            STATS.set_frequency(safe);
            seed_voltage(psu_driver, psu, BMM101_HASHBOARD.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let tuner = OnePlanTuner(Plan {
                frequencies: [Some(raised)],
                rail: None,
                delay: Duration::from_millis(500),
            });
            let driver = BringUpDriver {
                plan_log: Some((0, &LOG)),
                // Complete after the production cancellation budget expires.
                stop_during_frequency: Some((
                    *psu_driver.mining,
                    INTERRUPT_TIMEOUT + Duration::from_millis(50),
                )),
                ..BringUpDriver::new(&[4])
            };
            let mut subscription = test_config().0;
            let outcome = run_mining_with_driver(psu, &mut miner, tuner, &mut subscription, driver);
            assert!(outcome.is_none());
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Frequency(0, raised), Write::Disable,]
                );
            });
            assert_eq!(STATS.frequency(), raised);
        });
    }
}

/// Plans two empty steps, then a rail write on every step. `calls`
/// counts the steps where the test can read it after the machine has
/// dropped the tuner.
///
/// The tests build it themselves and start the machine from [`Step`], so
/// the machine never selects it.
struct WaitingTuner {
    calls: &'static AtomicU8,
    last_step: Option<Instant>,
}

impl WaitingTuner {
    fn new(calls: &'static AtomicU8) -> Self {
        calls.store(0, Ordering::Relaxed);
        Self {
            calls,
            last_step: None,
        }
    }
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for WaitingTuner {
    type PlatformReadings = ();

    // The miner never reads the list.
    const TUNERS: &'static [TunerInfo] = &[];

    fn select(_: &ubos_config::Tuning, _: &'static HashboardDescriptor) -> Option<Self> {
        panic!("the tests build this tuner themselves")
    }

    fn step(
        &mut self,
        _: &ubos_config::Tuning,
        readings: &Readings<CHAINS>,
        (): &(),
    ) -> Plan<CHAINS> {
        if let Some(last) = self.last_step {
            assert!(readings.now.as_millis() > last.as_millis());
        }
        assert_eq!(
            readings.chains[0].expect("chain").frequency,
            BMM101_HASHBOARD.chip.frequency_range.min()
        );
        assert_eq!(readings.voltage, BMM101_HASHBOARD.max_rail_voltage());
        self.last_step = Some(readings.now);
        let calls = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        Plan {
            rail: (calls >= 3).then_some(Voltage::from_mv(1100)),
            frequencies: [None; CHAINS],
            delay: Duration::from_millis(1),
        }
    }
}

/// Plans one rail step per call, a different voltage each time, so no
/// write is skipped as one the PSU already holds.
struct RailStepTuner {
    calls: u32,
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for RailStepTuner {
    type PlatformReadings = ();

    // The miner never reads the list.
    const TUNERS: &'static [TunerInfo] = &[];

    fn select(_: &ubos_config::Tuning, _: &'static HashboardDescriptor) -> Option<Self> {
        Some(Self { calls: 0 })
    }

    fn step(&mut self, _: &ubos_config::Tuning, _: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        self.calls += 1;
        Plan {
            rail: Some(Voltage::from_mv(1200_u32.saturating_sub(self.calls * 10))),
            frequencies: [None; CHAINS],
            delay: Duration::from_millis(1),
        }
    }
}

/// Plans a rail step on every other call and nothing in between, so a
/// run of refused commands is broken up by plans that ask for nothing.
struct HoldingRailTuner {
    calls: u32,
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for HoldingRailTuner {
    type PlatformReadings = ();

    // The miner never reads the list.
    const TUNERS: &'static [TunerInfo] = &[];

    fn select(_: &ubos_config::Tuning, _: &'static HashboardDescriptor) -> Option<Self> {
        Some(Self { calls: 0 })
    }

    fn step(&mut self, _: &ubos_config::Tuning, _: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        self.calls += 1;
        Plan {
            rail: (self.calls % 2 == 1)
                .then(|| Voltage::from_mv(1200_u32.saturating_sub(self.calls * 10))),
            frequencies: [None; CHAINS],
            delay: Duration::from_millis(1),
        }
    }
}

rusty_fork_test! {
    #[test]
    fn waiting_between_refused_commands_does_not_refresh_the_budget() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Fail),
            });
            STATS.set_presence(ChainPresence::Present {
                chips: BMM101_HASHBOARD.num_chips,
            });
            STATS.set_frequency(BMM101_HASHBOARD.chip.frequency_range.min());
            seed_voltage(psu_driver, psu, BMM101_HASHBOARD.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let state = MinerState::Step(Step {
                run: run(HoldingRailTuner { calls: 0 }),
            });
            let mut subscription = test_config().0;
            let stop_after_the_cut = async {
                core::future::poll_fn(|_| {
                    if LOG.lock(|log| log.borrow().contains(&Write::Disable)) {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                psu_driver.mining.stop();
            };
            let Either::First((outcome, ())) = block_on_with(
                psu,
                select(
                    join(
                        drive(&mut miner, state, &mut subscription),
                        stop_after_the_cut,
                    ),
                    Timer::after(Duration::from_millis(500)),
                ),
            ) else {
                panic!("waits between refused commands kept the miner off the fault path")
            };
            assert!(outcome.is_none());
            // The budget's worth of commands reached the PSU; the empty plans
            // between them did not clear the count.
            LOG.lock(|log| {
                let writes = log.borrow();
                let commands = writes
                    .iter()
                    .filter(|write| matches!(write, Write::Voltage(_)))
                    .count();
                assert_eq!(commands, usize::from(PLAN_FAILURE_BUDGET));
            });
        });
    }
}

rusty_fork_test! {
    #[test]
    fn a_plan_that_applies_clears_the_failure_budget() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            // One failure, one plan that applies, then two more failures.
            // Without the reset the last of those would spend the budget and
            // cut the rail.
            const SCRIPT: &[bool] = &[false, true, false, false];
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Script(SCRIPT)),
            });
            STATS.set_presence(ChainPresence::Present {
                chips: BMM101_HASHBOARD.num_chips,
            });
            STATS.set_frequency(BMM101_HASHBOARD.chip.frequency_range.min());
            seed_voltage(psu_driver, psu, BMM101_HASHBOARD.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let state = MinerState::Step(Step {
                run: run(RailStepTuner { calls: 0 }),
            });
            let mut subscription = test_config().0;
            let stop_once_the_script_is_spent = async {
                core::future::poll_fn(|_| {
                    if LOG.lock(|log| log.borrow().len()) > SCRIPT.len() {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                psu_driver.mining.stop();
            };
            let Either::First((outcome, ())) = block_on_with(
                psu,
                select(
                    join(
                        drive(&mut miner, state, &mut subscription),
                        stop_once_the_script_is_spent,
                    ),
                    Timer::after(Duration::from_millis(500)),
                ),
            ) else {
                panic!("the mining loop never reached the end of the script")
            };
            assert!(outcome.is_none());
            LOG.lock(|log| {
                let writes = log.borrow();
                let cut = writes
                    .iter()
                    .position(|write| *write == Write::Disable)
                    .expect("the stop cuts the rail");
                // Every scripted write, and then one more, must reach the
                // PSU before the rail goes: a cut at or before the script's
                // last entry is the failure budget counting across the plan
                // that applied.
                assert!(
                    cut > SCRIPT.len(),
                    "the rail was cut while the budget still had room"
                );
            });
        });
    }
}

rusty_fork_test! {
    #[test]
    fn a_configuration_naming_no_available_tuner_holds_with_the_rail_off() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            STATS.set_presence(ChainPresence::Present {
                chips: BMM101_HASHBOARD.num_chips,
            });
            let mut miner =
                test_miner::<crate::tuner::fixed::FixedTuner, 1>(psu_driver, psu, [&ACTOR], [&STATS]);
            let power = ubos_config::Tuning::Power(ubos_config::PowerTarget {
                input_power: ubos_core::units::Power::from_mw(3_000_000),
                efficiency: ubos_core::units::Efficiency::from_mj_per_th(core::num::NonZeroU32::MIN),
            });
            // The fixed tuner does not serve a power target, which is what
            // puts the machine in the hold.
            assert!(
                <crate::tuner::fixed::FixedTuner as MachineTuner<1>>::select(&power, &BMM101_HASHBOARD)
                    .is_none()
            );
            let (mut subscription, publish) = test_config();
            publish(ubos_config::Config {
                miner: ubos_config::MinerConfig { tuning: power },
                ..crate::fixture::default_config()
            });
            let halted = Cell::new(false);
            let machine = async {
                let mut state = MinerState::Choose(Choose);
                loop {
                    state = miner
                        .advance(state, &mut subscription)
                        .await
                        .expect("no stop is signalled");
                    halted.set(matches!(state, MinerState::Halted(_)));
                }
            };
            // More halt and resume edges than the command channel can hold.
            // A hold that never reads them blocks the manager task that
            // sends them, which is the whole of the pause aggregation.
            let pause_the_machine = async {
                for _ in 0..=crate::control::CMD_CHANNEL_DEPTH {
                    psu_driver.mining.halt().await;
                    psu_driver.mining.resume().await;
                }
                // The last edge is a halt, and a usable tuning that arrives
                // after it must find the machine still halted.
                psu_driver.mining.halt().await;
                core::future::poll_fn(|_| {
                    if crate::control::COMMAND_CHANNEL.is_empty() && halted.get() {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                publish(fixed_config(
                    Frequency::from_khz(500),
                    Voltage::from_mv(1200),
                ));
                Timer::after(Duration::from_millis(10)).await;
            };
            let Either::Second(Either::First(())) = block_on_with(
                psu,
                select(
                    machine,
                    select(pause_the_machine, Timer::after(Duration::from_millis(500))),
                ),
            ) else {
                panic!("the hold stopped reading the command channel")
            };
            assert!(
                halted.get(),
                "a halt raised during the hold was read and thrown away"
            );
            // Entry to the hold cuts the rail rather than trusting that an
            // earlier state left it down.
            LOG.lock(|log| assert!(log.borrow().contains(&Write::Disable)));
            // The hold is entered with the rail off, and a halted machine
            // does not bring it up for a new tuning.
            LOG.lock(|log| assert!(!log.borrow().contains(&Write::Enable)));
            while crate::control::COMMAND_CHANNEL.try_receive().is_ok() {}
        });
    }
}

rusty_fork_test! {
    #[test]
    fn empty_plans_wait_before_retrying_and_stop_during_a_write_cuts_the_rail() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            static CALLS: AtomicU8 = AtomicU8::new(0);
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Fail),
            });
            STATS.set_presence(ChainPresence::Present {
                chips: BMM101_HASHBOARD.num_chips,
            });
            STATS.set_frequency(BMM101_HASHBOARD.chip.frequency_range.min());
            seed_voltage(psu_driver, psu, BMM101_HASHBOARD.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let state = MinerState::Step(Step {
                run: run(WaitingTuner::new(&CALLS)),
            });
            let mut subscription = test_config().0;
            let stop_after_failure = async {
                core::future::poll_fn(|_| {
                    if LOG.lock(|log| log.borrow().contains(&Write::Disable)) {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                psu_driver.mining.stop();
            };
            let (outcome, ()) = block_on_with(
                psu,
                join(
                    drive(&mut miner, state, &mut subscription),
                    stop_after_failure,
                ),
            );
            assert!(outcome.is_none());
            // Two empty plans, then a rail write on every call until the
            // budget of failures runs out.
            assert_eq!(CALLS.load(Ordering::Relaxed), 2 + PLAN_FAILURE_BUDGET);
            // The stop arrives after the fault has cut the rail, so the rail
            // is cut once.
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow()
                        .iter()
                        .filter(|entry| **entry == Write::Disable)
                        .count(),
                    1
                );
            });

            STATS.set_presence(ChainPresence::Present {
                chips: BMM101_HASHBOARD.num_chips,
            });
            psu_driver.behavior.set(VoltageWrite::Interrupt);
            LOG.lock(|log| log.borrow_mut().clear());
            let outcome = run_mining_with_driver(
                psu,
                &mut miner,
                WaitingTuner::new(&CALLS),
                &mut subscription,
                BringUpDriver::new(&[4]),
            );
            assert!(outcome.is_none());
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Voltage(Voltage::from_mv(1100)), Write::Disable]
                );
            });
        });
    }
}

rusty_fork_test! {
    #[test]
    fn stop_before_work_initialization_prevents_voltage_commands() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            psu_driver.mining.stop();
            let state = MinerState::<OnePlanTuner>::StartWork(StartWork);
            assert!(block_on_with(psu, miner.advance(state, &mut subscription)).is_none());
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn settling_ignores_resume_and_preserves_a_resume_queued_after_halt() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            let mut miner = test_miner::<OnePlanTuner, 1>(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            {
                let mut settle = core::pin::pin!(beside_psu(
                    psu,
                    miner.advance(MinerState::Enable(Enable), &mut subscription)
                ));
                let mut context = Context::from_waker(Waker::noop());
                assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Resume).is_ok());
                assert!(settle.as_mut().poll(&mut context).is_pending());
                LOG.lock(|log| assert!(log.borrow().is_empty()));
                assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Halt).is_ok());
                assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Resume).is_ok());
                assert!(matches!(
                    poll_settled(settle.as_mut(), &mut context),
                    Poll::Ready(Some(MinerState::Halted(_)))
                ));
            }
            assert!(matches!(
                block_on_with(
                    psu,
                    miner.advance(MinerState::Halted(Halted::until_resume()), &mut subscription)
                ),
                Some(MinerState::Choose(_))
            ));
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn stop_ready_before_a_step_cuts_the_rail_without_a_plan() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            seed_voltage(psu_driver, psu, Voltage::from_mv(1000));
            let state = MinerState::Step(Step {
                run: run(OnePlanTuner(Plan {
                    rail: Some(Voltage::from_mv(1150)),
                    frequencies: [None],
                    delay: Duration::from_millis(500),
                })),
            });
            let mut subscription = test_config().0;
            psu_driver.mining.stop();
            assert!(block_on_with(psu, miner.advance(state, &mut subscription)).is_none());
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn stop_during_plan_delay_does_not_wait_or_issue_another_plan() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let voltage = Voltage::from_mv(1150);
            seed_voltage(psu_driver, psu, Voltage::from_mv(1000));
            let delay = Duration::from_millis(500);
            let state = MinerState::Step(Step {
                run: run(OnePlanTuner(Plan {
                    rail: Some(voltage),
                    frequencies: [None],
                    delay,
                })),
            });
            let mut subscription = test_config().0;
            let paused = block_on_with(psu, miner.advance(state, &mut subscription))
                .expect("no stop is signalled");
            assert!(matches!(paused, MinerState::Pause(_)));
            psu_driver.mining.stop();
            let Either::First(outcome) = block_on_with(
                psu,
                select(
                    miner.advance(paused, &mut subscription),
                    Timer::after(delay / 2),
                ),
            ) else {
                panic!("the stop waited for the plan delay")
            };
            assert!(outcome.is_none());
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Voltage(voltage), Write::Disable,]
                );
            });
        });
    }
}

rusty_fork_test! {
    #[test]
    fn a_tuner_change_during_the_plan_delay_cuts_the_rail_and_chooses_again() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let (mut subscription, publish) = test_config();
            block_on_with(psu, subscription.get());
            let state = MinerState::Pause(Pause {
                run: run(OnePlanTuner(Plan {
                    rail: None,
                    frequencies: [None],
                    delay: Duration::from_secs(1),
                })),
                until: Instant::now() + Duration::from_secs(1),
            });
            publish(ubos_config::Config {
                miner: ubos_config::MinerConfig {
                    tuning: ubos_config::Tuning::Power(ubos_config::PowerTarget {
                        input_power: ubos_core::units::Power::from_mw(3_000_000),
                        efficiency: ubos_core::units::Efficiency::from_mj_per_th(
                            core::num::NonZeroU32::MIN,
                        ),
                    }),
                },
                ..crate::fixture::default_config()
            });
            assert!(matches!(
                block_on_with(psu, miner.advance(state, &mut subscription)),
                Some(MinerState::Choose(_))
            ));
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn halt_finishes_the_pending_voltage_write_before_cutting_the_rail() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::YieldThenSucceed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let voltage = Voltage::from_mv(1100);
            seed_voltage(psu_driver, psu, Voltage::from_mv(1000));
            let state = MinerState::Step(Step {
                run: run(OnePlanTuner(Plan {
                    rail: Some(voltage),
                    frequencies: [None],
                    delay: Duration::from_millis(500),
                })),
            });
            let mut subscription = test_config().0;
            {
                let mut step =
                    core::pin::pin!(beside_psu(psu, miner.advance(state, &mut subscription)));
                let mut context = Context::from_waker(Waker::noop());
                // The voltage write yields once before it completes.
                assert!(step.as_mut().poll(&mut context).is_pending());
                block_on(psu_driver.mining.halt());
                block_on(psu_driver.mining.resume());
                assert!(matches!(
                    poll_settled(step.as_mut(), &mut context),
                    Poll::Ready(Some(MinerState::Halted(_)))
                ));
            }
            assert_eq!(psu.handle.commanded_voltage(), Some(voltage));
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Voltage(voltage), Write::Disable]
                );
            });
            assert!(matches!(
                block_on_with(
                    psu,
                    miner.advance(MinerState::Halted(Halted::until_resume()), &mut subscription)
                ),
                Some(MinerState::Choose(_))
            ));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn halt_during_stop_grace_period_keeps_stop_and_queued_resume() {
        use core::future::Future;
        use core::task::{Context, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Interrupt),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            seed_voltage(psu_driver, psu, Voltage::from_mv(1000));
            let state = MinerState::Step(Step {
                run: run(OnePlanTuner(Plan {
                    rail: Some(Voltage::from_mv(1100)),
                    frequencies: [None],
                    delay: Duration::from_millis(500),
                })),
            });
            let mut subscription = test_config().0;
            let mut context = Context::from_waker(Waker::noop());
            {
                let mut step =
                    core::pin::pin!(beside_psu(psu, miner.advance(state, &mut subscription)));
                // The voltage write signals STOP and never completes.
                assert!(step.as_mut().poll(&mut context).is_pending());
                // Consume STOP and enter the grace period with the write pending.
                assert!(step.as_mut().poll(&mut context).is_pending());
                block_on(psu_driver.mining.halt());
                block_on(psu_driver.mining.resume());
                assert!(step.as_mut().poll(&mut context).is_pending());
                assert!(matches!(
                    // The PSU task drops the stuck write one grace period
                    // after the disable queues behind it.
                    block_on(select(
                        step.as_mut(),
                        Timer::after(ubos_psu::COMMAND_GRACE * 5),
                    )),
                    Either::First(None)
                ));
            }
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow().as_slice(),
                    &[Write::Voltage(Voltage::from_mv(1100)), Write::Disable,]
                );
            });
            // The halt and the resume sent during the grace are still queued.
            assert!(matches!(
                block_on_with(psu, miner.mining.next_message()),
                Message::Halt
            ));
            assert!(matches!(
                block_on_with(psu, miner.mining.next_message()),
                Message::Resume
            ));
        });
    }
}

rusty_fork_test! {
    // A halt queued before the first [`Choose`] cuts the rail again, because
    // the boot cut only logs a failed disable and [`Halted`] holds until a
    // resume.
    #[test]
    fn a_halt_read_in_the_first_choose_cuts_the_rail_again() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            block_on_with(psu, psu_driver.mining.halt());
            let state = MinerState::<OnePlanTuner>::Choose(Choose);
            assert!(matches!(
                block_on_with(psu, miner.advance(state, &mut subscription)),
                Some(MinerState::Halted(_))
            ));
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        });
    }
}

/// A halt that reaches the miner in [`Reset`] publishes `Stopped` after the
/// rail cut, whether or not the supply answered the disable.
fn check_halt_after_reset(disable_fails: bool) {
    use crate::MiningState;

    critical_section::with(|_| {
        static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
        static STATS: ChainStats = ChainStats::new();
        static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
        let (runner, mining) = crate::Handle::take();
        let mut states = mining.subscribe();
        let mut subscription = test_config().0;
        let (psu_driver, psu) = test_psu(TestPsu {
            mining: TestMining::of(mining, runner),
            log: &LOG,
            behavior: Cell::new(VoltageWrite::Succeed),
            disable_fails,
        });
        let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
        miner.mining.publish(MiningState::Starting);
        block_on(mining.halt());
        assert_eq!(block_on(states.changed()), MiningState::Starting);
        let state = MinerState::<OnePlanTuner>::Reset(Reset);
        assert!(matches!(
            block_on_with(psu, miner.advance(state, &mut subscription)),
            Some(MinerState::Halted(_))
        ));
        assert_eq!(miner.chains[0].reset.log.as_slice(), &[ResetCall::Enter]);
        LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));
        assert_eq!(block_on(states.changed()), MiningState::Stopped);
    });
}

rusty_fork_test! {
    #[test]
    fn halt_publishes_stopped_after_reset_when_psu_disable_succeeds() {
        check_halt_after_reset(false);
    }

    #[test]
    fn halt_publishes_stopped_after_reset_even_if_psu_disable_fails() {
        check_halt_after_reset(true);
    }
}

/// Exercise the production policy with a shorter delay between steps.
struct LiveTuner(crate::tuner::fixed::FixedTuner);
impl MachineTuner<1> for LiveTuner {
    type PlatformReadings = ();

    const TUNERS: &'static [TunerInfo] =
        <crate::tuner::fixed::FixedTuner as MachineTuner<1>>::TUNERS;

    fn select(tuning: &ubos_config::Tuning, board: &'static HashboardDescriptor) -> Option<Self> {
        <crate::tuner::fixed::FixedTuner as MachineTuner<1>>::select(tuning, board).map(Self)
    }

    fn step(&mut self, tuning: &ubos_config::Tuning, readings: &Readings<1>, (): &()) -> Plan<1> {
        Plan {
            delay: Duration::from_millis(1),
            ..MachineTuner::step(&mut self.0, tuning, readings, &())
        }
    }
}

/// The tuner the default configuration selects, as the mining loop
/// records it. Every tuner in this file serves that variant.
fn fixed_selection() -> Selected {
    Selected::of(&crate::fixture::default_config().miner.tuning)
}

/// A configuration whose fixed target is `frequency` at `voltage`.
fn fixed_config(frequency: Frequency, voltage: Voltage) -> ubos_config::Config {
    ubos_config::Config {
        miner: ubos_config::MinerConfig {
            tuning: ubos_config::Tuning::Fixed(ubos_config::FrequencyVoltageTarget {
                frequency,
                voltage,
            }),
        },
        ..crate::fixture::default_config()
    }
}

rusty_fork_test! {
    #[test]
    fn live_frequency_and_voltage_edits_do_not_cut_power() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            static SESSION: AsicSession<1, 1> = AsicSession::new();
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let board = &BMM101_HASHBOARD;
            let safe = board.chip.frequency_range.min();
            STATS.set_presence(ChainPresence::Present {
                chips: board.num_chips,
            });
            STATS.set_frequency(safe);
            seed_voltage(psu_driver, psu, board.max_rail_voltage());
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let state = MinerState::Step(Step {
                run: run(LiveTuner(crate::tuner::fixed::FixedTuner::new(board))),
            });
            let (mut subscription, publish) = test_config();
            let mut frequency = safe;
            let mut voltage = board.max_rail_voltage();
            publish(fixed_config(frequency, voltage));
            let exercise = async {
                assert!(drive(&mut miner, state, &mut subscription).await.is_none());
                ACTOR.terminate().await.expect("actor terminates");
            };
            let edit = async {
                Timer::after(Duration::from_millis(1)).await;
                frequency = Frequency::from_mhz(75);
                publish(fixed_config(frequency, voltage));
                core::future::poll_fn(|_| {
                    if STATS.frequency() == Frequency::from_mhz(75) {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                LOG.lock(|log| assert!(!log.borrow().contains(&Write::Disable)));
                voltage = Voltage::from_mv(voltage.mv() - 100);
                publish(fixed_config(frequency, voltage));
                core::future::poll_fn(|_| {
                    if psu.handle.commanded_voltage() == Some(voltage) {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                })
                .await;
                LOG.lock(|log| assert!(!log.borrow().contains(&Write::Disable)));
                psu_driver.mining.stop();
            };
            let driver = BringUpDriver {
                plan_log: Some((0, &LOG)),
                ..BringUpDriver::new(&[4])
            };
            let run = ACTOR.run(
                crate::fixture::park(),
                QuietSerial,
                &SESSION,
                board,
                driver,
                NoWork,
                &STATS,
            );
            match block_on_with(
                psu,
                select(
                    join3(exercise, edit, run),
                    Timer::after(Duration::from_millis(500)),
                ),
            ) {
                Either::First(((), (), result)) => result.expect("actor succeeds"),
                Either::Second(()) => panic!("live target timed out"),
            }
            LOG.lock(|log| {
                assert_eq!(
                    log.borrow()
                        .iter()
                        .filter(|entry| **entry == Write::Disable)
                        .count(),
                    1
                );
            });
            assert_eq!(STATS.frequency(), Frequency::from_mhz(75));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn stop_has_priority_over_halt_in_every_state() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let states: [MinerState<OnePlanTuner>; 7] = [
                MinerState::Reset(Reset),
                MinerState::WaitForPool(WaitForPool),
                MinerState::Enable(Enable),
                MinerState::ExitReset(ExitReset),
                MinerState::BringUp(BringUp),
                MinerState::StartWork(StartWork),
                MinerState::Recover(Recover),
            ];
            for state in states {
                psu_driver.mining.stop();
                block_on_with(psu, psu_driver.mining.halt());
                assert!(block_on_with(psu, miner.advance(state, &mut subscription)).is_none());
                assert!(matches!(
                    block_on_with(psu, miner.mining.next_message()),
                    Message::Halt
                ));
            }
        });
    }
}

rusty_fork_test! {
    #[test]
    fn pool_watch_reports_loss_only_after_an_uninterrupted_grace() {
        let grace = Duration::from_secs(60);
        let start = Instant::from_secs(1_000);
        let mut watch = PoolWatch::default();
        assert!(!watch.observe(true, start, grace));
        assert!(!watch.observe(false, start, grace));
        assert!(!watch.observe(false, start + Duration::from_secs(59), grace));
        // A job in between restarts the count.
        assert!(!watch.observe(true, start + Duration::from_secs(59), grace));
        assert!(!watch.observe(false, start + Duration::from_secs(60), grace));
        assert!(!watch.observe(false, start + Duration::from_secs(119), grace));
        assert!(watch.observe(false, start + Duration::from_secs(120), grace));
    }
}

rusty_fork_test! {
    #[test]
    fn the_dangerous_flag_halts_a_state_and_its_clearing_alone_starts_over() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            let mut miner = test_miner::<OnePlanTuner, 1>(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let thermal = thermal_runner();
            thermal.publish_dangerous(true);
            let halted = block_on_with(
                psu,
                miner.advance(MinerState::Enable(Enable), &mut subscription),
            )
            .expect("a flag does not stop the machine");
            assert!(matches!(halted, MinerState::Halted(_)));
            LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));

            let mut held = core::pin::pin!(miner.advance(halted, &mut subscription));
            let mut context = Context::from_waker(Waker::noop());
            assert!(held.as_mut().poll(&mut context).is_pending());
            thermal.publish_dangerous(false);
            assert!(matches!(
                held.as_mut().poll(&mut context),
                Poll::Ready(Some(MinerState::Choose(_)))
            ));
        });
    }

    #[test]
    fn the_manager_cannot_resume_a_machine_the_flag_holds() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            let mut miner = test_miner::<OnePlanTuner, 1>(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let thermal = thermal_runner();
            thermal.publish_dangerous(true);
            let halted = MinerState::Halted(Halted::until_resume());
            let mut held = core::pin::pin!(miner.advance(halted, &mut subscription));
            let mut context = Context::from_waker(Waker::noop());
            assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Resume).is_ok());
            assert!(held.as_mut().poll(&mut context).is_pending());
            thermal.publish_dangerous(false);
            assert!(matches!(
                held.as_mut().poll(&mut context),
                Poll::Ready(Some(MinerState::Choose(_)))
            ));
        });
    }

    #[test]
    fn a_clear_flag_does_not_resume_a_machine_the_manager_holds() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                disable_fails: false,
                behavior: Cell::new(VoltageWrite::Succeed),
            });
            let mut miner = test_miner::<OnePlanTuner, 1>(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let thermal = thermal_runner();
            thermal.publish_dangerous(true);
            let halted = MinerState::Halted(Halted::while_dangerous());
            let mut held = core::pin::pin!(miner.advance(halted, &mut subscription));
            let mut context = Context::from_waker(Waker::noop());
            // The manager halts while only the flag holds the machine.
            assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Halt).is_ok());
            assert!(held.as_mut().poll(&mut context).is_pending());
            thermal.publish_dangerous(false);
            assert!(held.as_mut().poll(&mut context).is_pending());
            assert!(crate::control::COMMAND_CHANNEL.try_send(Message::Resume).is_ok());
            assert!(matches!(
                held.as_mut().poll(&mut context),
                Poll::Ready(Some(MinerState::Choose(_)))
            ));
        });
    }
}

fn a_job() -> stratum_client::WorkAssignment {
    stratum_client::WorkAssignment {
        assignment_id: 1,
        prev_hash: [0x11; 32],
        merkle_root: [0x33; 32],
        target: [0x22; 32],
        version: 0x2000_0000,
        version_mask: 0,
        nbits: 0x1705_c739,
        ntime: 0x6000_0000,
    }
}

rusty_fork_test! {
    #[test]
    fn the_pool_wait_holds_until_a_slot_holds_a_job_and_cuts_the_rail_on_halt() {
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static SESSION: AsicSession<1, 1> = AsicSession::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let waiting = || MinerState::<OnePlanTuner>::WaitForPool(WaitForPool);
            let exercise = async {
                // No job: the wait is still pending after several polls.
                let pending = select(
                    miner.advance(waiting(), &mut subscription),
                    Timer::after(POOL_POLL_PERIOD * 3),
                )
                .await;
                assert!(matches!(pending, Either::Second(())));
                LOG.lock(|log| assert!(log.borrow().is_empty()));

                SESSION.pool_handle(0).publish_work(a_job());
                let outcome = miner.advance(waiting(), &mut subscription).await;
                assert!(matches!(outcome, Some(MinerState::Enable(_))));
                LOG.lock(|log| assert!(log.borrow().is_empty()));

                SESSION.pool_handle(0).clear_work();
                psu_driver.mining.halt().await;
                let outcome = miner.advance(waiting(), &mut subscription).await;
                assert!(matches!(outcome, Some(MinerState::Halted(_))));
                assert_eq!(miner.chains[0].reset.log.as_slice(), &[ResetCall::Enter]);
                LOG.lock(|log| assert_eq!(log.borrow().as_slice(), &[Write::Disable]));

                ACTOR.terminate().await.expect("actor terminates");
            };
            let run = ACTOR.run(
                crate::fixture::park(),
                QuietSerial,
                &SESSION,
                &BMM101_HASHBOARD,
                BringUpDriver::new(&[4]),
                RatioScheduler::<1>::new(),
                &STATS,
            );
            let outcome = block_on_with(
                psu,
                select(join(exercise, run), Timer::after(Duration::from_secs(10))),
            );
            let Either::First(((), result)) = outcome else {
                panic!("pool wait timed out")
            };
            result.expect("actor succeeds");
        });
    }
}

/// The fee slot of the two-slot session the fee wait test runs, after its
/// one operator slot.
const FEE_POOL_SLOT: usize = 1;

rusty_fork_test! {
    #[test]
    fn the_pool_wait_ignores_a_fee_job_and_opens_on_an_operator_job() {
        if !DEVFEE {
            return;
        }
        critical_section::with(|_| {
            static ACTOR: Serial = Serial::new(HashboardIndex(0), 1);
            static STATS: ChainStats = ChainStats::new();
            static SESSION: AsicSession<2, 1> = AsicSession::new();
            static LOG: HardwareLog = Mutex::new(RefCell::new(heapless::Vec::new()));
            let (psu_driver, psu) = test_psu(TestPsu {
                mining: mining_handle(),
                log: &LOG,
                behavior: Cell::new(VoltageWrite::Succeed),
                disable_fails: false,
            });
            let mut miner = test_miner(psu_driver, psu, [&ACTOR], [&STATS]);
            let mut subscription = test_config().0;
            let waiting = || MinerState::<OnePlanTuner>::WaitForPool(WaitForPool);
            let exercise = async {
                // Only the fee pool holds a job: the wait stays pending, so
                // the rail stays off and no work reaches the chains.
                SESSION
                    .pool_handle(FEE_POOL_SLOT)
                    .publish_work(a_job());
                let pending = select(
                    miner.advance(waiting(), &mut subscription),
                    Timer::after(POOL_POLL_PERIOD * 3),
                )
                .await;
                assert!(matches!(pending, Either::Second(())));
                LOG.lock(|log| assert!(log.borrow().is_empty()));

                // The operator pool holds one too: the wait ends.
                SESSION.pool_handle(0).publish_work(a_job());
                let outcome = miner.advance(waiting(), &mut subscription).await;
                assert!(matches!(outcome, Some(MinerState::Enable(_))));

                ACTOR.terminate().await.expect("actor terminates");
            };
            let run = ACTOR.run(
                crate::fixture::park(),
                QuietSerial,
                &SESSION,
                &BMM101_HASHBOARD,
                BringUpDriver::new(&[4]),
                FeeScheduler::<2>::new(20),
                &STATS,
            );
            let outcome = block_on_with(
                psu,
                select(join(exercise, run), Timer::after(Duration::from_secs(10))),
            );
            let Either::First(((), result)) = outcome else {
                panic!("pool wait timed out")
            };
            result.expect("actor succeeds");
        });
    }
}

rusty_fork_test! {
    // The declared edges are sound: every state is reachable from [`Choose`]
    // and has a path of its own edges back to it, so no state is one that
    // only a halt can leave.
    #[test]
    fn every_state_is_reachable_and_leads_back_to_choose() {
        type TestContext = Context<'static, LoggingResetPin, OnePlanTuner, 1>;
        assert_eq!(
            transitions::check::<MinerState<OnePlanTuner>, TestContext>(
                <Choose as transitions::Named>::NAME
            ),
            Ok(())
        );
    }
}
