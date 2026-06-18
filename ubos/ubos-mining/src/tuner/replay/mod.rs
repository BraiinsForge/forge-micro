//! `ReplayTuner` — bring-up via the captured wire trace.
//!
//! Walks the compile-time-baked `INIT` and `DEINIT` byte sequences
//! step by step. Every byte to the chip goes through
//! [`crate::MinerControl`]'s byte-level escape hatches, so the tuner
//! never imports `crate::serial::SERIAL` directly.
//!
//! The captured trace is byte-level by design — it covers writes to
//! reserved registers and chip-link operations (baud change, raw
//! commands, work-send start) that no semantic API can express.

use embassy_time::Timer;
use embedded_io::ErrorKind;
use ubos_logging::{info, warn};

use crate::miner::{MinerControl, MinerError};
use ubos_asic::RegisterResponse;

use super::{Tuner, TunerError, TuningConfig};

pub(crate) mod trace;

use trace::{DEINIT, INIT, Step};

/// Bring-up and teardown via the captured wire trace.
///
/// `init` and `deinit` walk `INIT` and `DEINIT` respectively; every
/// chip-bound byte routes through the `MinerControl` trait. The
/// trait's byte-level escape hatches exist for this tuner.
pub struct ReplayTuner;

impl ReplayTuner {
    /// Register that returns the 32-bit chip ID. The boot trace probes
    /// this both unconditionally and after every baud-rate change.
    const CHIP_ID_REG: u8 = 0x00;

    /// Top 16 bits of the chip-ID register on a healthy BM1362.
    const EXPECTED_CHIP_ID: u32 = 0x1362;

    async fn walk<M: MinerControl>(miner: &M, steps: &[Step]) -> Result<(), TunerError> {
        for (index, step) in steps.iter().enumerate() {
            match step {
                Step::WriteRegister { addr, reg, value } => {
                    miner.write_register_raw(*addr, *reg, *value).await;
                }
                Step::ReadRegister { addr, reg } => {
                    let responses = miner.read_register_raw(*addr, *reg).await;
                    if *reg == Self::CHIP_ID_REG {
                        Self::verify_chip_ids(&responses);
                    }
                }
                Step::RawCommand(payload) => {
                    miner.send_raw_command(payload).await;
                }
                Step::SetBaudRate(rate) => {
                    miner.set_baud_rate(*rate).await?;
                    // Liveness check at the new line rate; the chip going
                    // silent here means the rest of the trace is moot.
                    let responses = miner
                        .read_register_raw(ubos_asic::ChipAddress::All, Self::CHIP_ID_REG)
                        .await;
                    if responses.is_empty() {
                        warn!(
                            "replay[{}]: no chip responses after baud change to {} baud",
                            index, rate
                        );
                        return Err(TunerError::Miner(MinerError::Serial(ErrorKind::TimedOut)));
                    }
                    Self::verify_chip_ids(&responses);
                }
                Step::Sleep(duration) => {
                    Timer::after(*duration).await;
                }
                Step::StartSendingWork(period) => {
                    miner.start_sending_work(*period).await?;
                }
            }
        }
        Ok(())
    }

    fn verify_chip_ids(responses: &[RegisterResponse]) {
        for response in responses {
            let chip_id = (response.value >> 16) & 0xFFFF;
            if chip_id == Self::EXPECTED_CHIP_ID {
                info!(
                    "chip 0x{:02X} confirmed: BM1362 (0x{:08X})",
                    response.chip_addr, response.value
                );
            } else {
                warn!(
                    "chip 0x{:02X} unexpected ID: 0x{:04X} (0x{:08X})",
                    response.chip_addr, chip_id, response.value
                );
            }
        }
    }
}

impl<M: MinerControl> Tuner<M> for ReplayTuner {
    async fn init(&mut self, miner: &M, target: TuningConfig) -> Result<(), TunerError> {
        // Voltage is no longer programmed by `Miner::run` ahead of
        // `init`; each tuner now owns its own voltage write so a
        // ramping tuner can anchor at open-core and walk down.
        // ReplayTuner has no ramp — it programs the configured target
        // up-front and the captured trace handles the rest.
        miner.set_voltage(target.voltage).await?;
        Self::walk(miner, INIT).await
    }

    async fn deinit(&mut self, miner: &M) -> Result<(), TunerError> {
        Self::walk(miner, DEINIT).await
    }
}
