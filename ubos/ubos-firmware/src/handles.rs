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
/// A board calls [`Handles::take`] once and gives each handle to the tasks
/// that read it or send to it, and the listeners get the whole set. The
/// takes are in one function because each handle can be taken once, so a
/// second place that takes one stops the boot at the second take.
#[derive(Clone, Copy)]
pub struct Handles {
    /// The temperature readings.
    pub thermal: ubos_thermal::Handle,
    /// The pause mask and the command channel of the manager.
    pub manager: ubos_manager::Handle,
    /// The fan telemetry.
    pub cooling: ubos_cooling::Handle,
    /// The mining lifecycle status and commands.
    pub mining: ubos_mining::Handle,
    /// The network identity and the request count.
    pub net: ubos_net::net::Handle,
    /// The persisted configuration.
    pub config: ubos_config::Handle,
    /// The commands and the readings of the power supply.
    pub psu: ubos_psu::Handle,
    /// The pool counters of the stratum sessions.
    pub stratum: stratum_client::PoolCountersHandle,
}

/// The right to run the task of each subsystem that a board runs.
///
/// Each runner except the network publisher is not `Copy`, so a board that
/// gives one to a task cannot give it to a second, and the compiler
/// rejects a second task for the same subsystem.
pub struct Runners {
    /// The sensor loop.
    pub thermal: ubos_thermal::Runner,
    /// The command router.
    pub manager: ubos_manager::Runner,
    /// The fan loop.
    pub cooling: ubos_cooling::Runner,
    /// The miner.
    pub mining: ubos_mining::Runner,
    /// The publisher of the network identity, which the board and two
    /// tasks share.
    pub net: ubos_net::net::Publisher,
    /// The config task.
    pub config: ubos_config::Runner,
    /// The power supply task.
    pub psu: ubos_psu::Runner,
}

impl Handles {
    /// Take every handle and every runner.
    ///
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
