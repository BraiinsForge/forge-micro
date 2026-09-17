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

//! ASIC mining main loop over the firmware-injected chain driver.
//!
//! One [`Miner`] owns the PSU rail and every chain of the machine.
//! Each bring-up iteration powers the rail up, resets every chain,
//! enumerates every chain at once, then runs `tuner.init` over the chains
//! that answered present. A connector with no board behind it, or one
//! whose chip count never settles on the board's expected value, is
//! parked in reset and left out of the ramp instead of stopping the
//! whole machine; only a bring-up where no chain is present cuts the
//! rail and retries.
//!
//! `Message::Halt` is the thermal-emergency path: when delivered the
//! loop stops work dispatch on every present chain and drops the rail,
//! then waits for `Message::Resume` before attempting bring-up again.
//! It runs no `tuner.deinit` ramp, because the board is already over
//! `dangerous_temp_c` and a ramp holds the rail up for the length of the
//! walk. `STOP_MINING` remains the orderly-shutdown path that runs
//! `tuner.deinit`.

use embassy_futures::join::join_array;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embedded_io::ErrorKind;
use ubos_asic::HashboardDescriptor;
use ubos_config::{ConfigSubscription, TuningAlgorithm};
use ubos_core::units::{Frequency, Voltage};
use ubos_logging::{Debug2Format, error, info, warn};
use ubos_psu::{Error as PsuError, Psu};

use crate::chain::presence::{self, Action};
use crate::chain::{ChainControl, ChainHandle, ChainPresence};
use crate::control::{COMMAND_CHANNEL, Message, MiningState, STATE_WATCH};
use crate::reset::ResetPin;
use crate::serial::Serial;
use crate::tuner::fixed::FixedTuner;
use crate::tuner::{Tuner, TuningConfig};

/// Raise once to stop the miner. The run loop awaits this between init
/// and deinit; signalling it triggers the teardown sequence and the
/// outer loop returns.
#[cfg_attr(feature = "ccmram-bss", unsafe(link_section = ".ccmram.bss"))]
pub static STOP_MINING: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Error type for chip-control operations.
///
/// Each variant represents a distinct failure mode the caller can
/// observe and dispatch on. `Serial` wraps the `embedded_io` error
/// kind so the miner can surface a consistent error surface without
/// coupling callers to the specific serial actor types.
#[derive(Debug)]
pub enum MinerError {
    /// The serial actor returned an error on a control operation.
    Serial(ErrorKind),
    /// The PSU rejected a voltage or enable/disable command. The
    /// payload narrows the failure to a discriminant the caller can
    /// dispatch on without taking on the PSU's `BusError` generic.
    Psu(PsuErrorKind),
    /// The chain driver rejected a frequency outside the chip's range.
    UnsupportedFrequency(Frequency),
}

impl From<ubos_asic::ChipError> for MinerError {
    fn from(error: ubos_asic::ChipError) -> Self {
        match error {
            ubos_asic::ChipError::UnsupportedFrequency(freq) => {
                MinerError::UnsupportedFrequency(freq)
            }
            // A refused host baud switch is a serial-line failure; the
            // rate itself is already logged by the transport.
            ubos_asic::ChipError::BaudSwitch(_) => MinerError::Serial(ErrorKind::Other),
        }
    }
}

impl From<embedded_io::ErrorKind> for MinerError {
    fn from(kind: embedded_io::ErrorKind) -> Self {
        MinerError::Serial(kind)
    }
}

/// Classification of [`ubos_psu::Error`] surfaces stripped of the
/// generic bus-error payload so [`MinerError`] does not have to be
/// parameterized over the PSU's `BusError` type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PsuErrorKind {
    Bus,
    InitFailed,
    OutOfRange,
    Fault,
    Terminated,
    Timeout,
}

impl<E> From<PsuError<E>> for PsuErrorKind {
    fn from(error: PsuError<E>) -> Self {
        match error {
            PsuError::Bus(_) => PsuErrorKind::Bus,
            PsuError::InitFailed(_) => PsuErrorKind::InitFailed,
            PsuError::OutOfRange => PsuErrorKind::OutOfRange,
            PsuError::Fault(_) => PsuErrorKind::Fault,
            PsuError::Terminated => PsuErrorKind::Terminated,
            PsuError::Timeout => PsuErrorKind::Timeout,
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for MinerError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            MinerError::Serial(kind) => {
                defmt::write!(f, "Serial({:?})", defmt::Debug2Format(kind));
            }
            MinerError::Psu(kind) => {
                defmt::write!(f, "Psu({:?})", defmt::Debug2Format(kind));
            }
            MinerError::UnsupportedFrequency(freq) => {
                defmt::write!(f, "UnsupportedFrequency({} kHz)", freq.khz());
            }
        }
    }
}

/// Machine-wide control surface used by `Tuner` implementations.
/// Implemented by [`Miner`]; defined as a trait so tuner
/// implementations do not need to be generic over the `(P, R, CHAINS)`
/// parameters of `Miner`.
///
/// Voltage is one value for the whole machine, set through the shared
/// PSU rail. Frequency and every other per-chain operation reach a
/// chain only through [`Self::Chain`], returned by
/// [`Self::present_chains`]; a tuner never addresses a chain by index.
// The async-fn-in-trait pattern matches the project-wide convention
// established by `ubos_psu::Psu`. Send bounds are not required on
// this embedded target; the trait is only called from a single-task
// context.
#[allow(async_fn_in_trait)]
pub trait MinerControl {
    /// The chain-facing handle this miner hands out. A tuner reaches a
    /// chain only through this type's `ChainControl` methods.
    type Chain: ChainControl;

    /// Set the core voltage through the PSU driver.
    async fn set_voltage(&self, voltage: Voltage) -> Result<(), MinerError>;

    /// Chains the most recent enumeration found present, in board
    /// order. A tuner ramps every item this iterator yields; a chain
    /// left out has no hashboard behind it or never settled on the
    /// board's expected chip count.
    fn present_chains(&self) -> impl Iterator<Item = &Self::Chain>;
}

/// The ASIC mining state machine driving the injected chain driver.
///
/// Owns the PSU reference and every chain of the machine. Firmware
/// constructs this with [`Miner::new`] and calls [`Miner::run`] to
/// enter the bring-up loop. The `'a` lifetime covers the PSU reference
/// which today is always `'static` at the firmware call site.
pub struct Miner<'a, P, R, const CHAINS: usize>
where
    P: Psu,
    R: ResetPin,
{
    psu: &'a P,
    board: &'static HashboardDescriptor,
    chains: [ChainHandle<R>; CHAINS],
    halted: bool,
}

impl<'a, P, R, const CHAINS: usize> Miner<'a, P, R, CHAINS>
where
    P: Psu,
    R: ResetPin,
{
    /// Settling time held between each reset / PSU transition during
    /// the bring-up sequence.
    const RESET_HOLD: Duration = Duration::from_secs(5);

    /// Pause between a failed bring-up and the next attempt. Long enough
    /// for the chip's rail to fully discharge and any transient cable /
    /// board glitch to clear.
    const RETRY_DELAY: Duration = Duration::from_secs(5);

    /// Construct a `Miner` from a PSU reference, the board descriptor
    /// whose chip facts drive the tuner, and one serial actor with its
    /// reset pin per chain of the machine, in chain order.
    ///
    /// A `Serial` declares its own index and its own view of the
    /// machine's chain count, and both are checked against its position
    /// in `chains`. The reset pin travels in the same pair as its actor,
    /// so a board names the two together and there is no second array
    /// whose order could drift from the first.
    ///
    /// The PSU is borrowed for lifetime `'a`; on all current firmware
    /// targets `'a` is `'static` because the PSU lives in a static.
    ///
    /// # Panics
    ///
    /// Panics if a `Serial` declares an index other than its position
    /// in `chains`, or a chain count other than `CHAINS`: an actor that
    /// counts the machine's chains differently partitions the version
    /// mask differently, and two chains would hash the same headers.
    /// Both fire in a release build, because a machine that is wired
    /// wrong must not mine. `CHAINS` above `ubos_machine::MAX_CHAINS`,
    /// which the per-chain accounting table has no slot for, fails to
    /// compile.
    pub fn new(
        psu: &'a P,
        board: &'static HashboardDescriptor,
        chains: [(&'static Serial, R); CHAINS],
    ) -> Self {
        const {
            assert!(
                CHAINS <= ubos_machine::MAX_CHAINS,
                "the machine has more chains than MAX_CHAINS"
            );
        }
        let chains = chains.map(|(serial, reset)| ChainHandle {
            serial,
            stats: &crate::chain::stats::CHAIN_STATS[serial.chain()],
            reset,
            presence: ChainPresence::Unknown,
        });
        for (position, chain) in chains.iter().enumerate() {
            assert_eq!(
                chain.serial.chain(),
                position,
                "BUG: chain {position} wired to a Serial that declares another index"
            );
            assert_eq!(
                chain.serial.chains(),
                CHAINS,
                "BUG: chain {position} wired to a Serial that counts another number of chains"
            );
        }
        crate::chain::stats::set_chain_count(CHAINS);
        Self {
            psu,
            board,
            chains,
            halted: false,
        }
    }

    /// Drive the mining work loop.
    ///
    /// Reads the initial config from `ubos_config::handle()` and holds a
    /// subscription across the outer bring-up loop. On each iteration the
    /// outer loop reads the current config, constructs the tuner selected
    /// by `MinerConfig::algorithm`, and calls `bring_up_cycle`. Returns
    /// when `STOP_MINING` fires; on bare metal that never happens, so the
    /// loop runs forever.
    ///
    /// # Algorithm-change invariant
    ///
    /// Switching tuner algorithms requires a full chip teardown:
    /// `tuner.deinit()` + PSU rail cut + reset, then the outer loop
    /// reconstructs the new tuner and runs `tuner.init()` from the
    /// post-reset baseline. No in-place tuner swap; the chip must be
    /// at a clean baseline before the new tuner's init walk begins.
    pub async fn run(&mut self) {
        // Publish the initial state so a fan task that started before us
        // sees `false` instead of waiting forever for the first edge.
        publish_running(false);

        let config_handle = ubos_config::handle();
        let mut config_subscription = config_handle.subscribe();

        // Block for the config task's first publish before reading a
        // target. This task is spawned before the config task and reaches
        // the read below with no intervening await, so `current()` would
        // otherwise fall back to `Config::default()` — the BMM101/BM1362
        // board's operating point, wrong for this chip. Returns
        // immediately once a config has been published. Consuming the
        // first change here also keeps `mine_phase`'s entry `changed()`
        // from firing a spurious restart against the just-read target.
        config_subscription.changed().await;

        // Park the rail off before the first reset, so every board
        // begins bring-up from a known-off supply regardless of what
        // the previous boot cycle left it in. A failure is logged and
        // bring-up goes on: the enable that follows the reset sets the
        // rail either way.
        if let Err(error) = self.psu.disable().await {
            warn!(
                "psu disable before the first reset failed: {:?}",
                Debug2Format(&error)
            );
        }

        loop {
            let snapshot = config_handle.current();
            let target = TuningConfig {
                frequency: Frequency::from_mhz(snapshot.tuning.frequency_mhz),
                voltage: Voltage::from_mv(snapshot.tuning.voltage_mv),
            };

            let outcome = match snapshot.miner.algorithm {
                TuningAlgorithm::Fixed => {
                    let mut tuner = FixedTuner::new(self.board);
                    self.bring_up_cycle(
                        target,
                        snapshot.miner.algorithm,
                        &mut tuner,
                        &mut config_subscription,
                    )
                    .await
                }
            };
            if outcome.is_stop() {
                return;
            }
        }
    }

    /// Reset and enumerate every chain, all at once, cancellable by
    /// STOP and HALT the same way `tuner_init_phase` is.
    ///
    /// A chain that answers zero chips is marked absent; one whose
    /// count never settles on `self.board.num_chips` within the retry
    /// budget is marked faulted. Either way the chain is parked in
    /// reset and left out of the ramp, and the other chains carry on:
    /// only a bring-up where no chain comes up present cuts the rail.
    async fn enumerate_phase(&mut self) -> InitPhase {
        let expected = self.board.num_chips;
        match select3(
            enumerate_all(&mut self.chains, expected),
            STOP_MINING.wait(),
            next_halt_change(),
        )
        .await
        {
            Either3::First(EnumerateOutcome::Done) => InitPhase::Done,
            Either3::First(EnumerateOutcome::NonePresent) => {
                error!("mining: no chain present; cutting rail and retrying");
                cut_rail(self.psu, &mut self.chains).await;
                match select3(
                    Timer::after(Self::RETRY_DELAY),
                    STOP_MINING.wait(),
                    next_halt_change(),
                )
                .await
                {
                    Either3::First(()) => InitPhase::Retry,
                    Either3::Second(()) => {
                        info!("mining: stop signalled during recovery");
                        InitPhase::Stop
                    }
                    Either3::Third(value) => InitPhase::Halt(value),
                }
            }
            Either3::Second(()) => {
                info!("mining: stop signalled during enumeration; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Stop
            }
            Either3::Third(value) => {
                warn!("mining: halt asserted during enumeration; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Halt(value)
            }
        }
    }

    /// Drive `tuner.init` with STOP and HALT cancellation.
    ///
    /// Any `tuner.init` error cuts the rail and applies a
    /// `Self::RETRY_DELAY` settle (cancellable) before reporting
    /// `Retry`. The live config subscription means an off-grid
    /// frequency or transient PSU fault can recover once the operator edits
    /// the target rather than permanently disabling mining. STOP and HALT
    /// also cut the rail before returning so the caller only dispatches on
    /// the variant.
    async fn tuner_init_phase<T>(&mut self, tuner: &mut T, target: TuningConfig) -> InitPhase
    where
        T: Tuner<Self>,
    {
        info!("mining: running tuner init");
        // The `&*self` shared borrow is held only for the duration of the
        // select; all match arms that follow can take mutable field refs.
        let init_result = select3(
            tuner.init(&*self, target),
            STOP_MINING.wait(),
            next_halt_change(),
        )
        .await;
        match init_result {
            Either3::First(Ok(())) => {
                crate::events::tuner_initialised();
                InitPhase::Done
            }
            Either3::First(Err(error)) => {
                error!(
                    "mining: tuner init error ({:?}); cutting rail and retrying",
                    Debug2Format(&error)
                );
                crate::events::tuner_fault();
                cut_rail(self.psu, &mut self.chains).await;
                match select3(
                    Timer::after(Self::RETRY_DELAY),
                    STOP_MINING.wait(),
                    next_halt_change(),
                )
                .await
                {
                    Either3::First(()) => InitPhase::Retry,
                    Either3::Second(()) => {
                        info!("mining: stop signalled during recovery");
                        InitPhase::Stop
                    }
                    Either3::Third(value) => InitPhase::Halt(value),
                }
            }
            Either3::Second(()) => {
                info!("mining: stop signalled during tuner init; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Stop
            }
            Either3::Third(value) => {
                warn!("mining: halt asserted during tuner init; cutting rail");
                cut_rail(self.psu, &mut self.chains).await;
                InitPhase::Halt(value)
            }
        }
    }

    /// One bring-up + mine cycle for a single tuner instance.
    ///
    /// Runs the PSU-reset-settle-enumerate-init sequence over every
    /// chain, then delegates to `mine_phase` for the steady-state
    /// select loop. Returns `CycleOutcome::Stop` when `STOP_MINING`
    /// fires; returns `CycleOutcome::Continue` on halt, halt-clear, a
    /// mining-target change (algorithm or frequency/voltage), or a
    /// transient init failure that has already cut the rail.
    async fn bring_up_cycle<T: Tuner<Self>>(
        &mut self,
        target: TuningConfig,
        current_algorithm: TuningAlgorithm,
        tuner: &mut T,
        config_subscription: &mut ConfigSubscription,
    ) -> CycleOutcome {
        if self.halted {
            info!("mining: halt active; awaiting clear");
            match wait_halt_clear_or_stop().await {
                Resumed::Cleared => {
                    info!("mining: halt cleared; restarting bring-up");
                    self.halted = false;
                }
                Resumed::Stopped => return CycleOutcome::Stop,
            }
        }

        // -- bring-up --
        // Voltage programming lives in the Tuner trait so a tuner
        // that ramps voltage can anchor at the chip's open-core
        // voltage and walk down to `target.voltage` as the PLL
        // climbs.

        info!("mining: entering reset");
        crate::events::miner_reset();
        for chain in &mut self.chains {
            chain.reset.enter_reset().await;
        }
        match settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await {
            Settled::Continue => {}
            Settled::Stop => return CycleOutcome::Stop,
            Settled::Halt(value) => {
                self.halted = value;
                return CycleOutcome::Continue;
            }
        }

        info!("mining: enabling PSU");
        crate::events::psu_enabled();
        if let Err(error) = self.psu.enable().await {
            warn!("psu enable failed: {:?}", Debug2Format(&error));
        }
        // Fans follow the rail, not the tuner init completion: the
        // chip is dissipating power as soon as the PSU is on, and
        // init can take long enough for thermals to climb.
        publish_running(true);
        match settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await {
            Settled::Continue => {}
            Settled::Stop => return CycleOutcome::Stop,
            Settled::Halt(value) => {
                self.halted = value;
                return CycleOutcome::Continue;
            }
        }

        info!("mining: exiting reset");
        for chain in &mut self.chains {
            chain.reset.exit_reset().await;
        }
        match settle_or_abort(Self::RESET_HOLD, self.psu, &mut self.chains).await {
            Settled::Continue => {}
            Settled::Stop => return CycleOutcome::Stop,
            Settled::Halt(value) => {
                self.halted = value;
                return CycleOutcome::Continue;
            }
        }

        match self.enumerate_phase().await {
            InitPhase::Done => {}
            InitPhase::Stop => return CycleOutcome::Stop,
            InitPhase::Halt(value) => {
                self.halted = value;
                return CycleOutcome::Continue;
            }
            InitPhase::Retry => return CycleOutcome::Continue,
        }

        match self.tuner_init_phase(tuner, target).await {
            InitPhase::Done => {}
            InitPhase::Stop => return CycleOutcome::Stop,
            InitPhase::Halt(value) => {
                self.halted = value;
                return CycleOutcome::Continue;
            }
            InitPhase::Retry => return CycleOutcome::Continue,
        }

        // Every present chain is enumerated, configured and clocked;
        // from here it only hashes. A board that addresses it
        // differently for the two moves its mode line now.
        for chain in &mut self.chains {
            if chain.presence.is_present() {
                chain.reset.enter_work_mode().await;
            }
        }

        self.mine_phase(current_algorithm, target, tuner, config_subscription)
            .await
    }

    /// Steady-state mining select loop entered after a successful tuner init.
    ///
    /// Runs until `STOP_MINING`, a halt edge, or an algorithm change.
    /// Algorithm changes trigger a full tuner deinit + rail cut before
    /// returning `Continue` so the outer loop can reconstruct under the
    /// new algorithm. Other config edits (tuning fields) are persisted
    /// in `Config` but only take effect at the next bring-up; there is
    /// no in-place ramp.
    async fn mine_phase<T: Tuner<Self>>(
        &mut self,
        current_algorithm: TuningAlgorithm,
        target: TuningConfig,
        tuner: &mut T,
        config_subscription: &mut ConfigSubscription,
    ) -> CycleOutcome {
        info!("mining: mining; awaiting STOP_MINING, Message, or config change");
        loop {
            match select3(
                STOP_MINING.wait(),
                next_halt_change(),
                config_subscription.changed(),
            )
            .await
            {
                Either3::First(()) => {
                    // Orderly teardown.
                    info!("mining: running tuner deinit");
                    let _ = tuner.deinit(&*self).await;
                    self.stop_present_chains().await;
                    cut_rail(self.psu, &mut self.chains).await;
                    info!("mining: stopped");
                    return CycleOutcome::Stop;
                }
                Either3::Second(true) => {
                    warn!("mining: halt asserted while mining; cutting rail");
                    self.stop_present_chains().await;
                    cut_rail(self.psu, &mut self.chains).await;
                    self.halted = true;
                    return CycleOutcome::Continue;
                }
                Either3::Second(false) => {
                    // Halt cleared while already mining; stale edge.
                    // Absorb it rather than tear down a healthy chip.
                }
                Either3::Third(new_config) => {
                    // Reprogram the ASIC when the operating target moved: an
                    // algorithm change, or a frequency/voltage edit. Ramp the
                    // chain gracefully down to the safe-boot anchor (deinit,
                    // fast — see DEINIT_SETTLE), then reset so the outer loop
                    // re-reads the new target and brings up from a clean
                    // baseline. Config edits that do not touch mining
                    // (cooling, pools, network) are left to their own tasks
                    // and do not restart the chip.
                    let tuning_changed = new_config.tuning.frequency_mhz != target.frequency.mhz()
                        || new_config.tuning.voltage_mv != target.voltage.mv();
                    if new_config.miner.algorithm != current_algorithm || tuning_changed {
                        info!("mining: mining target changed; ramping down to restart");
                        let _ = tuner.deinit(&*self).await;
                        self.stop_present_chains().await;
                        cut_rail(self.psu, &mut self.chains).await;
                        return CycleOutcome::Continue;
                    }
                }
            }
        }
    }

    /// Stop work dispatch and reset every chain the most recent
    /// enumeration found present. Called at both teardown sites in
    /// `mine_phase` before `cut_rail` puts every chain, present or
    /// not, back into hardware reset.
    ///
    /// The actor's reset clears the chain's published accounting, so a
    /// chain stopped here reads as `Unknown` until the next enumeration
    /// decides it again, while an absent or faulted chain keeps its
    /// verdict. That is deliberate: the thermal sensor reads the
    /// published presence to pick the chains it may treat as silent, and
    /// a stopped chain answers no status turn. Left published as present
    /// it would be read as a stalled chain and halt the machine it was
    /// just parked on.
    async fn stop_present_chains(&mut self) {
        for chain in &self.chains {
            if chain.presence.is_present() {
                let _ = chain.serial.stop_sending_work().await;
                let _ = chain.serial.reset().await;
            }
        }
    }
}

impl<P, R, const CHAINS: usize> MinerControl for Miner<'_, P, R, CHAINS>
where
    P: Psu,
    R: ResetPin,
{
    type Chain = ChainHandle<R>;

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), MinerError> {
        self.psu
            .set_voltage(voltage)
            .await
            .map_err(|error| MinerError::Psu(error.into()))
    }

    fn present_chains(&self) -> impl Iterator<Item = &Self::Chain> {
        self.chains
            .iter()
            .filter(|chain| chain.presence.is_present())
    }
}

/// Reset and enumerate every chain, concurrently. Free function
/// rather than a `Miner` method so it can sit as one arm of the
/// `select3` in `enumerate_phase` while `self.psu` stays available for
/// the other arms' `cut_rail` calls.
async fn enumerate_all<R: ResetPin, const CHAINS: usize>(
    chains: &mut [ChainHandle<R>; CHAINS],
    expected: u8,
) -> EnumerateOutcome {
    join_array(
        chains
            .each_mut()
            .map(|chain| enumerate_chain(chain, expected)),
    )
    .await;
    if chains.iter().any(|chain| chain.presence.is_present()) {
        EnumerateOutcome::Done
    } else {
        EnumerateOutcome::NonePresent
    }
}

/// Reset and enumerate one chain; the body of [`enumerate_all`] for a
/// single chain, run once per chain and concurrently with its siblings.
async fn enumerate_chain<R: ResetPin>(chain: &mut ChainHandle<R>, expected: u8) {
    let _ = chain.serial.reset().await;
    let mut attempt = 1u8;
    let presence = loop {
        let found = chain.serial.reset_and_enumerate().await;
        // Every attempt, so a chain that came up short of its
        // descriptor is readable from the event log rather than only
        // over the debug link.
        crate::events::chain_enumerated_chips(
            truncate_chain(chain.serial.chain()),
            found,
            expected,
        );
        let (presence, action) = presence::transition(found, expected, attempt);
        match action {
            Action::Continue | Action::Park => break presence,
            Action::Retry => {
                warn!(
                    "mining: chain {} enumerated {} chips, expected {} (attempt {})",
                    chain.serial.chain(),
                    found,
                    expected,
                    attempt
                );
                attempt += 1;
            }
        }
    };
    chain.presence = presence;
    chain.stats.set_presence(presence);
    match presence {
        ChainPresence::Present { .. } => {}
        ChainPresence::Absent => {
            chain.reset.enter_reset().await;
            crate::events::chain_absent(truncate_chain(chain.serial.chain()));
        }
        ChainPresence::Faulted { found } => {
            chain.reset.enter_reset().await;
            error!(
                "mining: chain {} has {} chips, expected {}; parking",
                chain.serial.chain(),
                found,
                expected
            );
            crate::events::chain_chip_count_mismatch(
                truncate_chain(chain.serial.chain()),
                found,
                expected,
            );
        }
        ChainPresence::Unknown => {
            unreachable!("BUG: enumeration loop exits only on Continue or Park")
        }
    }
}

/// Narrow a chain index to the `u8` the event catalog carries. Board
/// chain counts are single digits; this only guards against a future
/// board exceeding that.
fn truncate_chain(chain: usize) -> u8 {
    u8::try_from(chain).unwrap_or(u8::MAX)
}

/// Outcome of [`enumerate_all`].
enum EnumerateOutcome {
    /// At least one chain came up present. An absent or faulted chain
    /// elsewhere does not change this outcome; it is only parked.
    Done,
    /// No chain came up present.
    NonePresent,
}

/// Idle counterpart to [`Miner::run`] for deployments without a real chip.
///
/// Publishes the initial `MiningState` so subscribers see a defined value,
/// then drains the command channel forever. Used by host-runner when no PSU
/// is configured: thermal still publishes temperatures and the API still
/// renders state, while `halt`/`resume` edges from thermal are absorbed
/// instead of back-pressuring the sender once the channel fills.
pub async fn run_idle() -> ! {
    publish_running(false);
    info!("mining: idle loop active (no chip); commands will be dropped");
    loop {
        let _ = COMMAND_CHANNEL.receive().await;
    }
}

/// Publish a `MiningState` reflecting the rail flag. Frequency and
/// voltage carry forward from the prior publish so the tuner ramp
/// telemetry isn't clobbered by a halt edge.
fn publish_running(running: bool) {
    let prior = STATE_WATCH
        .try_get()
        .unwrap_or(crate::control::MiningState::IDLE);
    STATE_WATCH.sender().send(MiningState {
        running,
        frequency: prior.frequency,
        voltage: prior.voltage,
    });
}

/// Block until a `Message::Halt`/`Resume` arrives. Returns the
/// resulting halt state.
async fn next_halt_change() -> bool {
    match COMMAND_CHANNEL.receive().await {
        Message::Halt => true,
        Message::Resume => false,
    }
}

/// Put every chain in reset and drop the rail, then report them stopped.
///
/// The order matters. Reset is asserted before the rail goes, so the
/// chips are held rather than losing power mid-transaction. A failure to
/// disable the PSU is logged and not returned: the reset is already
/// asserted, and a board that cannot drop its rail is in no better state
/// for the caller dispatching on the error here. The log line is what a
/// rail that stayed up leaves behind.
///
/// The PSU and the chain handles are two generic parameters of equal
/// standing, and neither is the receiver. No inherent `impl` in this
/// crate can name either, and the caller owns both for the life of the
/// mining loop.
async fn cut_rail<P, R>(psu: &P, chains: &mut [ChainHandle<R>])
where
    P: Psu,
    R: ResetPin,
{
    for chain in chains.iter_mut() {
        chain.reset.enter_reset().await;
    }
    if let Err(error) = psu.disable().await {
        warn!("psu disable failed: {:?}", Debug2Format(&error));
    }
    publish_running(false);
}

/// Outcome returned by `bring_up_cycle` to the outer `run` loop.
enum CycleOutcome {
    /// The outer loop must return: `STOP_MINING` was signalled.
    Stop,
    /// The outer loop should restart with a fresh config snapshot:
    /// a halt/resume cycle or an algorithm change triggered teardown.
    Continue,
}

impl CycleOutcome {
    fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

enum Resumed {
    Cleared,
    Stopped,
}

/// Block while halted. Returns `Cleared` once a `Message::Resume`
/// edge is observed; returns `Stopped` if `STOP_MINING` fires first.
async fn wait_halt_clear_or_stop() -> Resumed {
    loop {
        match select(next_halt_change(), STOP_MINING.wait()).await {
            Either::First(false) => return Resumed::Cleared,
            Either::First(true) => {}
            Either::Second(()) => return Resumed::Stopped,
        }
    }
}

/// Outcome of the enumeration or tuner init phase. Mirrors the three
/// actions the run loop must take afterwards plus a `Retry` for a
/// transient failure that has already cut the rail.
enum InitPhase {
    Done,
    Stop,
    Halt(bool),
    Retry,
}

/// Outcome of a cancellable settling sleep in the bring-up sequence.
enum Settled {
    /// The timer elapsed without interruption; the caller proceeds
    /// to the next bring-up step.
    Continue,
    /// `STOP_MINING` fired during the sleep; the rail has been cut
    /// and the run loop must return.
    Stop,
    /// A `Message::Halt` edge fired during the sleep; the rail has
    /// been cut and the carried value is the new `halted` state for
    /// the run loop to resume from.
    Halt(bool),
}

/// Cancellable settling sleep used between bring-up steps. STOP or
/// HALT raised during a 5-second hold must short-circuit so the rail
/// does not keep climbing toward `enable_PSU` after a thermal trip.
/// `Stop` and `Halt` outcomes drop the rail before returning so the
/// caller only needs to dispatch on the variant.
///
/// The PSU and the chain handles are two generic parameters of equal
/// standing, and neither is the receiver, the same shape as
/// [`cut_rail`], which this function calls on both abort paths.
async fn settle_or_abort<P, R>(
    duration: Duration,
    psu: &P,
    chains: &mut [ChainHandle<R>],
) -> Settled
where
    P: Psu,
    R: ResetPin,
{
    match select3(
        Timer::after(duration),
        STOP_MINING.wait(),
        next_halt_change(),
    )
    .await
    {
        Either3::First(()) => Settled::Continue,
        Either3::Second(()) => {
            cut_rail(psu, chains).await;
            Settled::Stop
        }
        Either3::Third(value) => {
            cut_rail(psu, chains).await;
            Settled::Halt(value)
        }
    }
}

/// Task shell that runs the [`Miner`] bring-up and teardown state
/// machine. Takes the board's PSU and reset-pin types, the descriptor
/// whose chip facts drive the tuner, and the machine's chain count.
///
/// [`Miner::run`] returns only when `STOP_MINING` fires, which on bare
/// metal never happens. The shell parks afterwards for the same reason
/// `ubos_mining::serial_task` does.
#[macro_export]
macro_rules! miner_task {
    ($psu:ty, $reset:ty, $board:expr, $chains:expr) => {
        #[embassy_executor::task]
        pub async fn miner_task(
            psu: &'static $psu,
            chains: [(&'static ubos_mining::serial::Serial, $reset); $chains],
        ) -> ! {
            /// Park interval after the state machine returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            let mut miner = ubos_mining::Miner::new(psu, $board, chains);
            miner.run().await;
            defmt::info!("ubos-mining miner exited");
            loop {
                embassy_time::Timer::after(PARK_INTERVAL).await;
            }
        }
    };
}
