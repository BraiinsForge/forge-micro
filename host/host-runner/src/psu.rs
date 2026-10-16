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

//! Workspace-static `BmmPsu` over the Linux i2c-dev bus.
//!
//! Activated by `UBOS_I2C_BUS=/dev/i2c-N`; without the env var set
//! `open()` returns `None` and the runner skips the miner spawn. The
//! `'static` reference [`open`] returns is handed to the watchdog and
//! miner tasks by `main.rs`.
//!
//! The host harness is not one of the boards and has no
//! `MachineDescriptor`; it initializes from `BmmPsuDescriptor::DEFAULT`,
//! the same const the miniminer machines wrap, so it cannot drift
//! from the firmware.

use embedded_i2c_unix::I2c as HostI2c;
use log::{error, info};
use static_cell::StaticCell;
use ubos_psu::{BmmPsu, BmmPsuDescriptor, BmmShared, PowerSupply};

/// Env var that points at the i2c-dev character device wired to the
/// BMM101 PSU. Unset means no hashboard is attached and the runner
/// skips the miner.
pub const I2C_BUS_ENV: &str = "UBOS_I2C_BUS";

pub type HostPsu = BmmPsu<HostI2c>;

static SHARED: StaticCell<BmmShared<HostI2c>> = StaticCell::new();
static PSU: StaticCell<HostPsu> = StaticCell::new();

/// Open the I2C bus from `UBOS_I2C_BUS`, stand up the PSU state, and
/// return the `'static` handle the miner / watchdog tasks need.
/// Returns `None` if the env var is unset/empty or the bus cannot be
/// opened.
pub fn open() -> Option<&'static HostPsu> {
    let env_path = std::env::var(I2C_BUS_ENV);
    let path = env_path.as_deref().unwrap_or("/dev/i2c-0");
    if path.is_empty() {
        return None;
    }
    let i2c = match HostI2c::open(path) {
        Ok(i2c) => i2c,
        Err(error) => {
            error!("psu: cannot open {path}: {error}");
            return None;
        }
    };
    info!("psu: opened {path}");
    let shared: &'static _ = SHARED.init(BmmShared::new(i2c, BmmPsuDescriptor::DEFAULT));
    Some(PSU.init(BmmPsu::new(shared)))
}

#[embassy_executor::task]
pub async fn watchdog_task(psu: &'static HostPsu) {
    psu.run_watchdog().await;
}

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static HostPsu) -> ! {
    PowerSupply::new(psu).run().await
}
