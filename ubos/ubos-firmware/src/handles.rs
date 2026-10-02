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

//! The handles of the subsystems that a board takes in `main`, and the
//! runners of the tasks that own them.

/// One handle for each subsystem that a board runs.
///
/// The takes share one function because each handle can be taken only once,
/// see `ubos_core::take_once`. A board gives each handle to the tasks that
/// use it, and the listeners get the whole set.
#[derive(Clone, Copy)]
pub struct Handles {
    pub thermal: ubos_thermal::Handle,
    pub manager: ubos_manager::Handle,
    pub cooling: ubos_cooling::Handle,
    pub mining: ubos_mining::Handle,
    pub net: ubos_net::net::Handle,
    pub config: ubos_config::Handle,
    pub psu: ubos_psu::Handle,
    pub stratum: stratum_client::PoolCountersHandle,
}

/// The right to run the task of each subsystem that a board runs.
///
/// Each runner except the network publisher, which the board and two tasks
/// share, is not `Copy`, so the compiler rejects a second task for one
/// subsystem.
pub struct Runners {
    pub thermal: ubos_thermal::Runner,
    pub manager: ubos_manager::Runner,
    pub cooling: ubos_cooling::Runner,
    pub mining: ubos_mining::Runner,
    pub net: ubos_net::net::Publisher,
    pub config: ubos_config::Runner,
    pub psu: ubos_psu::Runner,
}

impl Handles {
    /// # Panics
    ///
    /// Panics when any handle was taken before.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runners, Self) {
        let (thermal_runner, thermal) = ubos_thermal::Handle::take();
        let (manager_runner, manager) = ubos_manager::Handle::take();
        let (cooling_runner, cooling) = ubos_cooling::Handle::take();
        let (mining_runner, mining) = ubos_mining::Handle::take();
        let (net_publisher, net) = ubos_net::net::Handle::take();
        let (config_runner, config) = ubos_config::Handle::take();
        let (psu_runner, psu) = ubos_psu::Handle::take();
        let runners = Runners {
            thermal: thermal_runner,
            manager: manager_runner,
            cooling: cooling_runner,
            mining: mining_runner,
            net: net_publisher,
            config: config_runner,
            psu: psu_runner,
        };
        let handles = Self {
            thermal,
            manager,
            cooling,
            mining,
            net,
            config,
            psu,
            stratum: stratum_client::PoolCountersHandle::take(),
        };
        (runners, handles)
    }
}
