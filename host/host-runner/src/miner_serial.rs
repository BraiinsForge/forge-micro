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

//! Brings up the ubos-mining BM1362 serial actor against a Linux tty.
//!
//! Opens `/dev/ttySTM1` at the chip's reset baud rate and runs the
//! actor's server loop on the serial port. Failure to open the device
//! parks the task instead of crashing the runner so the rest of the
//! tasks (network, stratum, metrics) keep working when no hashboard is
//! wired in.

use embassy_time::{Duration, Timer};
use embedded_serial_unix::Serial;
use log::{error, info};
use ubos_mining::consts::INITIAL_BAUDRATE;

use crate::ASIC_SESSION;
use crate::config::{CHIP, MINER_SERIAL_PATH};

/// The runner's single hashboard chain. The count is a literal because the
/// runner has no machine descriptor to read it from: it drives one serial
/// device, named by `MINER_SERIAL_PATH`, and stands for no product.
pub static CHAINS: [ubos_mining::serial::Serial; 1] = ubos_mining::serial::Serial::table();

#[embassy_executor::task]
pub async fn miner_serial_task() -> ! {
    let serial = match Serial::open(MINER_SERIAL_PATH, INITIAL_BAUDRATE) {
        Ok(serial) => serial,
        Err(error) => {
            error!(
                "host-runner: failed to open {MINER_SERIAL_PATH}: {error}; miner serial disabled"
            );
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    info!("host-runner: miner serial bringup on {MINER_SERIAL_PATH}");
    // Single-slot session, so the ratio scheduler degenerates to
    // always serving slot 0 when it has work. `new`'s fallback
    // default already weights slot 0, so no reweight is needed.
    let scheduler = ubos_mining::scheduler::RatioScheduler::new();
    match CHAINS[0]
        .run(
            serial,
            &ASIC_SESSION,
            crate::BOARD,
            ubos_asic::Bm13xxDriver::new(CHIP),
            scheduler,
            ubos_mining::chain_stats::handle().chain(0),
        )
        .await
    {
        Ok(()) => info!("host-runner: miner serial actor terminated cleanly"),
        Err(kind) => error!("host-runner: miner serial actor exited: {kind:?}"),
    }
    // Park: the serial line is gone (EOF, IO error) or the actor was
    // told to stop. Re-opening would need fresh peripheral state which
    // the host doesn't have a clean trigger for here, so the rest of
    // the runner keeps running while this task idles.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
