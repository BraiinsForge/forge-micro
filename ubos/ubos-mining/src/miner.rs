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

//! Multi-chain mining lifecycle and execution of synchronous tuner plans.
//!
//! Bring-up holds the chains in reset with the rail off until the
//! scheduler would dispatch work, then configures every chain concurrently
//! and parks absent or faulted connectors. A run in which the scheduler
//! would dispatch nothing for `POOL_LOSS_GRACE` cuts the rail and waits for
//! a pool again.
//!
//! The miner applies bounded policy plans in decrease, rail, increase order.
//! Live targets take effect at the next step. A stop or a halt allows the current
//! plan a bounded wait, then cuts the rail without a shutdown ramp.
//!
//! [`Miner::run`] is a state machine on the `transitions` crate: each state
//! is a type whose `run` does its work and returns the next state, and
//! `Miner::advance` runs one state at a time under the machine's only select.

mod error;
mod machine;
mod pool;
mod states;
mod temperature;

#[cfg(test)]
#[path = "tests/miner.rs"]
mod tests;

use core::cmp::Ordering;

use embassy_time::{Duration, Instant};
use ubos_asic::HashboardDescriptor;
use ubos_core::units::{Frequency, Temperature, Voltage};
use ubos_logging::{Debug2Format, info, warn};
use ubos_metrics::chain::ChainPresence;
use ubos_psu::Handle as PsuHandle;
use ubos_tuner::{ChainReadings, Plan, Readings};

use crate::chain::{ChainControl, ChainHandle};
use crate::control::{MiningState, Runner};
use crate::reset::ResetPin;
use crate::serial::Serial;
use crate::tuner::MachineTuner;

pub use error::{MinerError, PsuErrorKind};

/// Bring-up cycles the machine spends while no chain comes up
/// present, before it parks instead of cutting the rail again. It is the
/// outer budget over each cycle's own `ENUMERATE_ATTEMPTS`: it buys the
/// rail cut a chain-level retry cannot perform, and bounds it, because a
/// machine with nothing behind its connectors would otherwise cycle the
/// rail for as long as it is powered.
const BRING_UP_CYCLES: u8 = 3;

/// Run the mining module of a deployment that has no chip: publish
/// [`MiningState::Stopped`] and drain halt and resume.
///
/// The command channel is bounded, so a deployment that never reads it
/// blocks every later sender, which is the manager task.
pub async fn run_idle(runner: Runner) -> ! {
    runner.publish(MiningState::Stopped);
    info!("mining: idle loop active (no chip)");
    loop {
        runner.next_message().await;
    }
}

/// The ASIC mining state machine driving the injected chain driver. `T`
/// is the machine's tuner set.
pub struct Miner<R, T, const CHAINS: usize>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    psu_handle: PsuHandle,
    board: &'static HashboardDescriptor,
    chains: [ChainHandle<R>; CHAINS],
    /// Readings only this board's platform has. Read once per step and
    /// passed to the tuner.
    read_platform: fn() -> T::PlatformReadings,
    /// The mining handle the API reads; the miner publishes its lifecycle
    /// status and reads halt and resume through it.
    mining: Runner,
    /// Where the miner reads the dangerous temperature flag and whether a
    /// reading is left.
    thermal: ubos_thermal::Handle,
    /// Bring-up cycles that found no chain since the last one that did.
    /// Counts toward `BRING_UP_CYCLES`, and is cleared by every cycle
    /// that brings a chain up and by every resume.
    empty_cycles: u8,
}

impl<R, T, const CHAINS: usize> Miner<R, T, CHAINS>
where
    R: ResetPin,
    T: MachineTuner<CHAINS>,
{
    /// Construct a `Miner` from the PSU handle, the board descriptor
    /// whose chip facts drive the tuner, one serial actor with its reset
    /// pin per chain of the machine, in chain order, and the function that
    /// reads what the platform reports beyond the common readings.
    ///
    /// # Panics
    ///
    /// Panics if a `Serial` declares an index other than its position
    /// in `chains`, or a chain count other than `CHAINS`: an actor that
    /// counts the machine's chains differently partitions the version
    /// mask differently, and two chains would hash the same headers.
    /// Both fire in a release build, because a machine that is wired
    /// wrong must not mine. `CHAINS` above `ubos_metrics::chain::MAX_CHAINS`,
    /// which the per-chain accounting table has no slot for, fails to
    /// compile.
    pub fn new(
        psu_handle: PsuHandle,
        board: &'static HashboardDescriptor,
        chains: [(&'static Serial, R); CHAINS],
        read_platform: fn() -> T::PlatformReadings,
        mining: Runner,
        thermal: ubos_thermal::Handle,
    ) -> Self {
        const {
            assert!(
                CHAINS <= ubos_metrics::chain::MAX_CHAINS,
                "the machine has more chains than MAX_CHAINS"
            );
        }
        // Checked before the accounting slots are taken, not after: the
        // lookup below indexes the table by the actor's own declared
        // index, so an actor declaring one past the table would panic
        // inside the slice and never reach the assertion that names the
        // wiring mistake.
        for (position, (serial, _)) in chains.iter().enumerate() {
            assert_eq!(
                serial.chain().slot(),
                position,
                "BUG: chain {position} wired to a Serial that declares another index"
            );
            assert_eq!(
                serial.chains(),
                CHAINS,
                "BUG: chain {position} wired to a Serial that counts another number of chains"
            );
        }
        let chains = chains.map(|(serial, reset)| ChainHandle {
            serial,
            stats: ubos_metrics::chain::handle().chain(serial.chain()),
            reset,
        });
        ubos_metrics::chain::set_chain_count(CHAINS);
        Self {
            psu_handle,
            board,
            chains,
            read_platform,
            mining,
            thermal,
            empty_cycles: 0,
        }
    }
    /// Count one bring-up cycle that brought no chain up, and report
    /// whether that spends `BRING_UP_CYCLES`.
    ///
    /// Free of I/O, so the budget is testable without the bring-up around it.
    fn empty_cycle_spends_the_budget(&mut self) -> bool {
        self.empty_cycles = self.empty_cycles.saturating_add(1);
        self.empty_cycles >= BRING_UP_CYCLES
    }

    /// The actor that answers pool questions for the machine. Every
    /// chain shares one session, so chain 0 speaks for all of them, and
    /// the question touches no chip, so its own presence does not matter.
    fn pool_serial(&self) -> &'static Serial {
        self.chains[0].serial
    }

    /// Put every chain in reset and drop the rail.
    ///
    /// The order matters. Reset is asserted before the rail goes, so the
    /// chips are held rather than losing power mid-transaction. A failure to
    /// disable the PSU is logged; the chips remain in reset.
    ///
    /// A chain that was present is recorded as `Unknown` here: its
    /// enumeration is void once the reset is asserted, and the next
    /// bring-up decides it again. An absent or faulted chain keeps its
    /// verdict, so a parked connector still reads as parked while the
    /// machine is down. The store is a direct write rather than an actor
    /// command, so a cut never waits behind a bring-up command the actor
    /// is still running.
    async fn cut_rail(&mut self) {
        for chain in &mut self.chains {
            chain.reset.enter_reset().await;
            if chain.is_present() {
                chain.stats.set_presence(ChainPresence::Unknown);
            }
        }
        if let Err(error) = self.psu_handle.disable().await {
            warn!("psu disable failed: {:?}", Debug2Format(&error));
        }
    }

    /// Set the open-core rail and start work only on configured chains.
    async fn start_chains(&self) -> Result<(), MinerError> {
        const WORK_PERIOD: Duration = Duration::from_secs(1);
        self.psu_handle
            .set_voltage(self.board.max_rail_voltage())
            .await
            .map_err(|error| MinerError::Psu(error.into()))?;
        for chain in self.chains.iter().filter(|chain| chain.is_present()) {
            chain.start_sending_work(WORK_PERIOD).await?;
        }
        Ok(())
    }

    /// Collect each present chain's accounting, the PSU's confirmed voltage,
    /// and `dangerous_chip_temp`: the configured die temperature at which
    /// mining halts.
    ///
    /// The setpoint is present by construction. `start_chains` commands
    /// the open-core rail and [`StartWork`] leads to
    /// [`Step`] only after that command succeeded, and the PSU task
    /// records the voltage a successful command set before it answers.
    fn readings(&self, dangerous_chip_temp: Option<Temperature>) -> Readings<CHAINS> {
        Readings {
            now: Instant::now(),
            voltage: self
                .psu_handle
                .commanded_voltage()
                .expect("BUG: tuning starts only after a successful PSU voltage command"),
            psu: self.psu_handle.current(),
            dangerous_chip_temp,
            chains: self.chains.each_ref().map(|chain| {
                chain.is_present().then(|| ChainReadings {
                    chips: chain.stats.chips(),
                    frequency: chain.stats.frequency(),
                    hottest: self.thermal.chains().hottest(chain.chain()),
                    hashrate: chain.stats.hashrate(),
                })
            }),
        }
    }

    /// Decreasing clocks precede the shared voltage change; increasing clocks follow it.
    ///
    /// Reports whether any command reached the hardware. A frequency for a
    /// connector with no chain, or a value the chain or the PSU already
    /// holds, is not a write.
    async fn apply_plan(&self, plan: &Plan<CHAINS>) -> Result<bool, MinerError> {
        let mut decreases = [None; CHAINS];
        let mut increases = [None; CHAINS];
        for (index, chain) in self.chains.iter().enumerate() {
            if !chain.is_present() {
                continue;
            }
            let Some(next) = plan.frequencies[index] else {
                continue;
            };
            let range = self.board.chip.frequency_range;
            let next = next.clamp(range.min(), range.max());
            match next.cmp(&chain.stats.frequency()) {
                Ordering::Less => decreases[index] = Some(next),
                Ordering::Greater => increases[index] = Some(next),
                Ordering::Equal => {}
            }
        }

        let slowed = self.set_frequencies(decreases).await?;
        // Only the ceiling is clamped here: it is board policy that can sit
        // above the PSU's own range, so nothing below needs to refuse it.
        // The floor is the PSU's, which returns `Error::OutOfRange` for a
        // rail under its range rather than reaching the hardware.
        let rail = self
            .set_voltage_if_different(
                plan.rail
                    .map(|rail| rail.min(self.board.max_rail_voltage())),
            )
            .await?;
        let quickened = self.set_frequencies(increases).await?;
        Ok(slowed || rail || quickened)
    }

    /// Reports whether any chain was written.
    async fn set_frequencies(
        &self,
        frequencies: [Option<Frequency>; CHAINS],
    ) -> Result<bool, MinerError> {
        let mut written = false;
        for (chain, next) in self.chains.iter().zip(frequencies) {
            if let Some(next) = next {
                chain.set_frequency(next).await?;
                written = true;
            }
        }
        Ok(written)
    }

    /// Reports whether the rail was written.
    async fn set_voltage_if_different(&self, voltage: Option<Voltage>) -> Result<bool, MinerError> {
        let Some(voltage) =
            voltage.filter(|next| Some(*next) != self.psu_handle.commanded_voltage())
        else {
            return Ok(false);
        };
        self.psu_handle
            .set_voltage(voltage)
            .await
            .map_err(|error| MinerError::Psu(error.into()))?;
        Ok(true)
    }
}
/// Task shell that runs the [`Miner`], over the board's types, because a
/// task cannot be generic.
///
/// [`Miner::run`] returns only when [`Handle::stop`] fires, which on bare
/// metal never happens. The shell parks afterwards for the same reason
/// `ubos_mining::serial_task` does.
#[macro_export]
macro_rules! miner_task {
    ($reset:ty, $board:expr, $chains:expr, $tuner:ty) => {
        ubos_mining::miner_task!($reset, $board, $chains, $tuner, || ());
    };
    ($reset:ty, $board:expr, $chains:expr, $tuner:ty, $platform:expr) => {
        #[embassy_executor::task]
        pub async fn miner_task(
            psu_handle: ubos_psu::Handle,
            chains: [(&'static ubos_mining::serial::Serial, $reset); $chains],
            mining: ubos_mining::Runner,
            thermal: ubos_thermal::Handle,
            config: ubos_config::Handle,
        ) -> ! {
            /// Park interval after the state machine returns.
            const PARK_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(60);

            let mut miner = ubos_mining::Miner::<_, $tuner, { $chains }>::new(
                psu_handle, $board, chains, $platform, mining, thermal,
            );
            miner.run(config).await;
            defmt::info!("ubos-mining miner exited");
            loop {
                embassy_time::Timer::after(PARK_INTERVAL).await;
            }
        }
    };
}
