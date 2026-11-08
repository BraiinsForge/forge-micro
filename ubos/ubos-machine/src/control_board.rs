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

//! Control-board descriptors.

use crate::platform::{ESP32S3, PlatformDescriptor, RP2040, RP2350};

/// The board the firmware runs on.
///
/// Carries the platform, the MAC family byte, the HTTP pool sizes, and
/// the event-log ring capacity. Realization details that a
/// trait already abstracts stay out: the pin and peripheral wiring is a
/// type-level concern the binary expresses directly, and the fan PWM
/// mechanism is abstracted by the `ubos_cooling::FanOutput` trait.
#[derive(Clone, Copy)]
pub struct ControlBoardDescriptor {
    /// Product name reported to external clients.
    pub name: &'static str,
    /// The MCU this board is built around.
    pub platform: PlatformDescriptor,
    /// MAC address family byte. The runtime folds a per-unit id into the
    /// remaining octets so each board type self-assigns stable, distinct
    /// MACs on a shared segment.
    pub mac_family: u8,
    /// Number of concurrent connections the HTTP listener serves. One
    /// listener answers the web UI and the REST API, and a connection
    /// that stays open holds its slot, so this is the ceiling on
    /// simultaneous connections rather than on clients. Count two
    /// connections for each browser and one for each machine client.
    ///
    /// Two is the floor on every board. A single slot puts the OTA
    /// recovery path behind whatever worker an open dashboard holds, and
    /// that is the path a wedged board is recovered over. It also leaves
    /// a board with an open page unreachable to every other client,
    /// because the 2 s poll holds the one worker.
    ///
    /// A slot costs more than the buffers it names. Every slot must be
    /// able to serve the REST API, whose handler future is far larger
    /// than the dashboard's, so a merged slot is dearer than either of
    /// the two per-listener slots it replaces.
    pub http_pool: usize,
    /// Number of concurrent connections the metrics HTTP listener serves.
    pub metrics_http_pool: usize,
    /// Capacity of the retained event-log ring.
    pub event_log_capacity: usize,
}

/// The W6100-EVB-Pico control board. The HTTP pools and the 32-entry
/// event ring are sized down for this RAM-constrained board.
pub const CONTROL_BOARD_RP2040: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "W6100-EVB-Pico",
    platform: RP2040,
    mac_family: 0x01,
    http_pool: 2,
    metrics_http_pool: 1,
    event_log_capacity: 32,
};

/// The RP2350 control board: a genuine Wiznet W6100-EVB-Pico2 module.
pub const CONTROL_BOARD_RP2350: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "W6100-EVB-Pico2",
    platform: RP2350,
    mac_family: 0x02,
    http_pool: 8,
    metrics_http_pool: 5,
    event_log_capacity: 128,
};

/// A genuine Wiznet W5500-EVB-Pico2 module mounted on Braiins' own "BMM
/// Adapter" carrier PCB, which wires it into the miniminer's
/// hashboard/PSU header. The carrier is an interposer, not the control
/// board product itself, so the reported name names the actual module.
pub const CONTROL_BOARD_BMM_ADAPTER: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "W5500-EVB-Pico2",
    platform: RP2350,
    mac_family: 0x03,
    http_pool: 8,
    metrics_http_pool: 5,
    event_log_capacity: 128,
};

/// A Waveshare ESP32-S3-LCD-Driver-Board with an external W5100S wired
/// through its LCD header. The pool lives in `.bss`, which shares
/// `dram_seg` with the core-0 stack, so an oversized pool starves the
/// boot stack and trips the esp-hal stack-guard during
/// `embassy_net::new`. The esp32s3 `build.rs` link-time ASSERT enforces
/// the resulting stack floor, and 4 is the deepest merged pool that
/// clears it. That is above the 3 either listener held here before the
/// merge, and below the 6 the two of them held together.
pub const CONTROL_BOARD_ESP32S3: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "Waveshare ESP32-S3-LCD-Driver-Board W5100S",
    platform: ESP32S3,
    mac_family: 0x05,
    http_pool: 4,
    metrics_http_pool: 3,
    event_log_capacity: 128,
};

/// The Hashnode: control board and hashboard are one integrated product,
/// so the name matches `ubos_asic::BFMX4_HASHBOARD.model` rather than
/// naming a separate control-board PCB. Reuses the RP2350 platform: the
/// Hashnode's RP2350B is a pin-compatible variant of the same Cortex-M33
/// core and target as the RP2350A the rp2350 and bmm-pico boards use.
pub const CONTROL_BOARD_HASHNODE: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "Braiins Forge Miner x4",
    platform: RP2350,
    mac_family: 0x04,
    http_pool: 8,
    metrics_http_pool: 5,
    event_log_capacity: 128,
};

/// The Forge Axe: control board and hashboard are one integrated
/// product, so the name matches `ubos_asic::FORGE_AXE_HASHBOARD.model`.
/// Reuses the RP2350 platform: the Forge Axe's RP2350B has the same
/// Cortex-M33 core and target as the RP2350A the rp2350 and bmm-pico
/// boards use, and differs only in package and GPIO count.
pub const CONTROL_BOARD_FORGE_AXE: ControlBoardDescriptor = ControlBoardDescriptor {
    name: "Forge Axe",
    platform: RP2350,
    mac_family: 0x08,
    http_pool: 8,
    metrics_http_pool: 5,
    event_log_capacity: 128,
};
