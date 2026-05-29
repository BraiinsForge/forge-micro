//! Walk a [`Step`] slice and dispatch each entry to the serial actor.
//!
//! Used by the miner main loop to play the BMM100 boot and teardown
//! sequences. Errors from baud changes and work-start propagate; reads
//! and writes are best-effort (the actor logs internally).

use embassy_time::Timer;
use embedded_io::ErrorKind;
use ubos_logging::{info, warn};

use crate::bm1362::{RawChipAddress, RegisterResponse, build_raw_command};
use crate::bmm100_trace::Step;
use crate::serial::Serial;

/// Register that returns the 32-bit chip ID. The boot trace probes
/// this both unconditionally and after every baud-rate change.
const CHIP_ID_REG: u8 = 0x00;

/// Top 16 bits of the chip-ID register on a healthy BM1362.
const EXPECTED_CHIP_ID: u32 = 0x1362;

pub async fn replay(serial: &Serial, steps: &[Step]) -> Result<(), ErrorKind> {
    for (index, step) in steps.iter().enumerate() {
        match step {
            Step::WriteRegister { addr, reg, value } => {
                serial.write_register(*addr, *reg, *value).await;
            }
            Step::ReadRegister { addr, reg } => {
                let responses = serial.read_register(*addr, *reg).await;
                if *reg == CHIP_ID_REG {
                    verify_chip_ids(&responses);
                }
            }
            Step::RawCommand(payload) => {
                serial.send_frame(build_raw_command(payload)).await;
            }
            Step::SetBaudRate(rate) => {
                serial.set_baud_rate(*rate).await?;
                // Liveness check at the new line rate; the chip going
                // silent here means the rest of the init sequence is
                // moot, so abort and let the caller recover.
                let responses = serial.read_register(RawChipAddress::All, CHIP_ID_REG).await;
                if responses.is_empty() {
                    warn!(
                        "replay[{}]: no chip responses after baud change to {}",
                        index, rate
                    );
                    return Err(ErrorKind::TimedOut);
                }
                verify_chip_ids(&responses);
            }
            Step::Sleep(duration) => {
                Timer::after(*duration).await;
            }
            Step::StartSendingWork(period) => {
                serial.start_sending_work(*period).await?;
            }
        }
    }
    Ok(())
}

fn verify_chip_ids(responses: &[RegisterResponse]) {
    for r in responses {
        let chip_id = (r.value >> 16) & 0xFFFF;
        if chip_id == EXPECTED_CHIP_ID {
            info!(
                "chip 0x{:02X} confirmed: BM1362 (0x{:08X})",
                r.chip_addr, r.value
            );
        } else {
            warn!(
                "chip 0x{:02X} unexpected ID: 0x{:04X} (0x{:08X})",
                r.chip_addr, chip_id, r.value
            );
        }
    }
}
