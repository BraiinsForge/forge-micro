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

//! The Ethernet MAC device type and its construction macro, shared by both
//! boards' built-in MAC with an RMII PHY over the onboard MDIO bus.

use embassy_stm32::eth::{Ethernet, GenericPhy, Sma};
use embassy_stm32::peripherals::{ETH, ETH_SMA};

/// The Ethernet driver type: the built-in MAC with an auto-probed PHY over
/// the onboard MDIO bus.
pub type Device = Ethernet<'static, ETH, GenericPhy<Sma<'static, ETH_SMA>>>;

/// Construct the [`Device`] from the RMII pin set. Takes the peripheral, the
/// interrupt binding, the RMII pins, the MAC address and the MDIO pins as
/// tokens, because their concrete types are board-local `Peri` bindings.
///
/// ```ignore
/// let device = ubos_stm32::ethernet_device!(
///     eth = hw.eth,
///     irqs = Irqs,
///     ref_clk = hw.ref_clk,
///     crs_dv = hw.crs_dv,
///     rxd0 = hw.rxd0,
///     rxd1 = hw.rxd1,
///     txd0 = hw.txd0,
///     txd1 = hw.txd1,
///     tx_en = hw.tx_en,
///     mac_addr = mac_addr,
///     eth_sma = hw.eth_sma,
///     mdio = hw.mdio,
///     mdc = hw.mdc,
/// );
/// ```
#[macro_export]
macro_rules! ethernet_device {
    (
        eth = $eth:expr,
        irqs = $irqs:expr,
        ref_clk = $ref_clk:expr,
        crs_dv = $crs_dv:expr,
        rxd0 = $rxd0:expr,
        rxd1 = $rxd1:expr,
        txd0 = $txd0:expr,
        txd1 = $txd1:expr,
        tx_en = $tx_en:expr,
        mac_addr = $mac_addr:expr,
        eth_sma = $eth_sma:expr,
        mdio = $mdio:expr,
        mdc = $mdc:expr $(,)?
    ) => {{
        static PACKETS: static_cell::StaticCell<embassy_stm32::eth::PacketQueue<4, 4>> =
            static_cell::StaticCell::new();
        embassy_stm32::eth::Ethernet::new(
            PACKETS.init(embassy_stm32::eth::PacketQueue::<4, 4>::new()),
            $eth,
            $irqs,
            $ref_clk,
            $crs_dv,
            $rxd0,
            $rxd1,
            $txd0,
            $txd1,
            $tx_en,
            $mac_addr,
            $eth_sma,
            $mdio,
            $mdc,
        )
    }};
}
