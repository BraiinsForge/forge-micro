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

//! One chain of the machine: its control surface, its presence, and
//! its accounting.
//!
//! [`ChainHandle`] is the miner's own view of a chain: the serial
//! actor, the accounting slot, the reset pin and the last enumeration
//! outcome, walked together through bring-up and teardown. The miner
//! applies tuner plans through [`ChainControl`], which the submodule
//! [`control`] defines. [`presence`] is the pure decision
//! table the enumeration loop drives. The per-chain accounting the API,
//! the display and the metrics publisher read is `ubos_metrics::chain`.

use embassy_time::{Duration, Timer};
use ubos_core::units::Frequency;
use ubos_logging::{Debug2Format, error, warn};

use crate::miner::MinerError;
use crate::reset::ResetPin;
use crate::serial::Serial;
use ubos_metrics::chain::{ChainPresence, ChainStats};

pub mod control;
pub mod presence;

pub(crate) use control::ChainControl;

use presence::Action;

/// How long a chain is held in reset between two bring-up attempts.
/// Shorter than `Miner::RESET_HOLD`, which settles a whole machine's
/// rail: here the rail is up and only one chain restarts.
const RETRY_RESET_HOLD: Duration = Duration::from_millis(100);

/// How long a chain is left alone after the reset release before the
/// next enumerate. A chip that answered nothing on the attempt before
/// was observed answering the attempt that followed a settle of this
/// order, so the retry is given the time the first try did not have.
/// The same value as `Miner::RETRY_DELAY`, which settles the machine
/// after a failed bring-up.
const RETRY_SETTLE: Duration = Duration::from_secs(5);

/// One chain's serial actor, accounting, and reset pin, bundled behind
/// the [`ChainControl`] surface the miner drives.
///
/// The reset pin is folded in here, rather than left in a parallel
/// array on `Miner`, because the miner always has to walk a chain's
/// reset pin and its serial actor together during bring-up and
/// teardown.
///
/// The chain's presence lives in its [`ChainStats`] and nowhere else:
/// the miner decides it and every reader, the miner included, reads it
/// back from there, so what the tuner ramps and what the API and the
/// thermal loop see can never be two different answers.
pub struct ChainHandle<R: ResetPin> {
    pub(crate) serial: &'static Serial,
    pub(crate) stats: &'static ChainStats,
    pub(crate) reset: R,
}

impl<R: ResetPin> ChainHandle<R> {
    /// True for a chain the most recent enumeration found present, read
    /// from the published accounting.
    pub(crate) fn is_present(&self) -> bool {
        self.stats.presence().is_present()
    }

    /// Reset, enumerate and configure this chain, retrying to the
    /// budget, and publish the presence the attempts settled on.
    ///
    /// Each attempt re-arms the line at the boot baud, resets and
    /// enumerates the chain, and, when the count matches, configures it
    /// to its safe-boot operating point on the fast UART and reads every
    /// chip's temperature and core voltage once, so the thermal loop is
    /// not blind through the ramp that follows. A short count, a refused
    /// configuration, or a sweep no chip answered spends one attempt of
    /// the same budget. Between two attempts the chain's hardware reset
    /// is pulsed and the line is re-armed at the boot baud, so the next
    /// attempt starts from the chip's own boot state: a refused
    /// configuration can leave the chip on the fast UART, where a
    /// boot-baud enumerate reads no chips and the decision table would
    /// file a real board as absent.
    pub(crate) async fn bring_up(&mut self, expected: u8) {
        let index = truncate_chain(self.chain());
        let mut attempt = 1u8;
        let presence = loop {
            let _ = self.serial.reset().await;
            let found = self.serial.reset_and_enumerate().await;
            // Every attempt, so a chain that came up short of its
            // descriptor is readable from the event log rather than only
            // over the debug link.
            crate::events::chain_enumerated_chips(index, found, expected);
            let (presence, action) = match presence::transition(found, expected, attempt) {
                (presence, Action::Continue) => match self.configure().await {
                    Ok(()) => (presence, Action::Continue),
                    Err(error) => {
                        warn!(
                            "mining: chain {} refused its configuration ({:?}) (attempt {})",
                            self.chain(),
                            Debug2Format(&error),
                            attempt
                        );
                        crate::events::chain_configure_failed(index, attempt);
                        presence::configure_failed(found, attempt)
                    }
                },
                decided => decided,
            };
            match action {
                Action::Continue | Action::Park => break presence,
                Action::Retry => {
                    if found != expected {
                        warn!(
                            "mining: chain {} enumerated {} chips, expected {} (attempt {})",
                            self.chain(),
                            found,
                            expected,
                            attempt
                        );
                    }
                    // The re-arm at the top of the next attempt moves
                    // only the host side of the line. The chip keeps
                    // whatever the failed attempt left it in, so it is
                    // pulsed through reset here and given `RETRY_SETTLE`
                    // to come back before it is spoken to again. The
                    // driver's own retries absorb a chip that boots
                    // slower than that settle.
                    self.reset.enter_reset().await;
                    Timer::after(RETRY_RESET_HOLD).await;
                    self.reset.exit_reset().await;
                    Timer::after(RETRY_SETTLE).await;
                    attempt += 1;
                }
            }
        };
        self.stats.set_presence(presence);
        match presence {
            ChainPresence::Present { .. } => {}
            ChainPresence::Absent => {
                self.reset.enter_reset().await;
                crate::events::chain_absent(index);
            }
            ChainPresence::Faulted { found } => {
                self.reset.enter_reset().await;
                if found == expected {
                    error!(
                        "mining: chain {} refused its configuration on every attempt; parking",
                        self.chain()
                    );
                } else {
                    error!(
                        "mining: chain {} has {} chips, expected {}; parking",
                        self.chain(),
                        found,
                        expected
                    );
                    crate::events::chain_chip_count_mismatch(index, found, expected);
                }
            }
            ChainPresence::Unknown => {
                unreachable!("BUG: bring-up loop exits only on Continue or Park")
            }
        }
    }

    /// Configure this enumerated chain to its safe-boot operating point
    /// on the fast UART, then read every chip's temperature and core
    /// voltage once.
    ///
    /// A sweep that no chip answered fails here rather than passing, so
    /// the caller spends an attempt on it. Presence is what the thermal
    /// loop measures a chain's silence from, and a chain that reached
    /// `Present` with nothing read would have the loop hold the reading
    /// from before its last rail cut through the whole ramp.
    async fn configure(&self) -> Result<(), MinerError> {
        self.serial.configure().await?;
        self.serial.pvt_sweep().await?;
        Ok(())
    }
}

/// Narrow a chain index to the `u8` the event catalog carries. Board
/// chain counts are single digits; this only guards against a future
/// board exceeding that.
fn truncate_chain(chain: usize) -> u8 {
    u8::try_from(chain).unwrap_or(u8::MAX)
}

/// The serial actor answers in the transport's own error types; this
/// impl is where they become the one [`MinerError`] the miner handles.
impl<R: ResetPin> ChainControl for ChainHandle<R> {
    fn chain(&self) -> usize {
        self.serial.chain()
    }

    /// Program the hash-clock frequency. The serial actor records it
    /// in this chain's accounting on success.
    async fn set_frequency(&self, freq: Frequency) -> Result<(), MinerError> {
        self.serial
            .set_frequency(freq)
            .await
            .map_err(MinerError::from)
    }

    async fn start_sending_work(&self, period: Duration) -> Result<(), MinerError> {
        self.serial
            .start_sending_work(period)
            .await
            .map_err(MinerError::from)
    }
}
