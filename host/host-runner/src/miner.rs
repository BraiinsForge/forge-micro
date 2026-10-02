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

//! Drives `ubos_mining::Miner` against the gpiocdev-backed reset pin
//! and the workspace-static `BmmPsu`.

use embassy_time::{Duration, Timer};
use log::{error, info};

use crate::gpiod::{GpiodResetPin, RESET_LINE};
use crate::psu::HostPsu;

#[embassy_executor::task]
pub async fn miner_task(psu: &'static HostPsu) -> ! {
    let reset_pin = match GpiodResetPin::open(RESET_LINE) {
        Ok(pin) => pin,
        Err(error) => {
            error!("host-runner: failed to open reset gpio: {error}; miner disabled");
            ubos_mining::miner::run_idle().await
        }
    };
    info!("host-runner: miner bringup");
    let mut miner = ubos_mining::Miner::<_, _, ubos_mining::tuner::Tuners, 1>::new(
        psu,
        crate::BOARD,
        [(&crate::miner_serial::CHAINS[0], reset_pin)],
        || (),
    );
    miner.run().await;
    info!("host-runner: miner exited; shutting down");
    Timer::after(Duration::from_secs(1)).await;
    std::process::exit(0);
}

#[embassy_executor::task]
pub async fn miner_idle_task() -> ! {
    ubos_mining::miner::run_idle().await
}
