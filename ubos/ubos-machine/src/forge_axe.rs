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

//! The Forge Axe boards, by the `hw_version` that their EEPROMs carry,
//! the machine they compose, and the sizes that the firmware fixes at
//! build time for whichever boards those are.
//!
//! Each lookup takes the record of its board and gives the descriptor that
//! the record names, with no I/O of its own, so a firmware only reads the
//! records and the checks run in the host tests.
//!
//! The lookups and their checks are free functions: the record they take
//! and two of the descriptors they give are types of other crates, and the
//! four lookups keep one shape.

use ubos_asic::HashboardDescriptor;
use ubos_eeprom::forge::{self, ControlBoardEeprom, HardwareType, Record};
use ubos_psu::ForgeAxePsuDescriptor;
use ubos_temperature::SensorDescriptor;

use crate::control_board::{CONTROL_BOARD_FORGE_AXE, ControlBoardDescriptor, HttpPoolSizes};
use crate::front_panel::{FRONT_PANEL_FORGE_AXE, FrontPanelDescriptor};
use crate::machine::{FORGE_AXE_PSU, MAX_CHAINS, MachineDescriptor};

/// How many hashboard chains a Forge Axe drives.
pub const FORGE_AXE_CHAINS: usize = 1;
const _: () = assert!(
    FORGE_AXE_CHAINS <= MAX_CHAINS,
    "the Forge Axe has more chains than MAX_CHAINS"
);

/// The HTTP worker pools of a Forge Axe.
// NOTE: every control board that the lookup gives takes these sizes,
// because the firmware fixes its pools at build time.
pub const FORGE_AXE_HTTP_POOL_SIZES: HttpPoolSizes = HttpPoolSizes {
    http: 8,
    metrics: 5,
};

/// Capacity of the retained event-log ring of a Forge Axe.
// NOTE: every control board that the lookup gives takes this capacity,
// because the firmware sizes the ring at build time.
pub const FORGE_AXE_EVENT_LOG_CAPACITY: usize = 128;

/// The I2C addresses of the Forge EEPROMs of a Forge Axe, one on each
/// board.
pub struct ForgeAxeEeprom;
impl ForgeAxeEeprom {
    pub const PSU: u8 = 0x0B;
    pub const HASHBOARD: u8 = 0x57;
    pub const CONTROL_BOARD: u8 = 0x50;
    pub const FRONT_PANEL: u8 = 0x51;
}

// TODO: the product code of each Forge Axe board replaces its placeholder
// `hw_version` in the lookups below.

/// The Forge Axe hashboard that `record` names.
///
/// # Errors
///
/// Returns a [`BoardError`] when `record` is not of a hashboard, names no
/// known hashboard, or holds data in its reserved bytes.
// NOTE: each hashboard here differs from `FORGE_AXE_HASHBOARD` only in its
// sensors, because the firmware drives the chips by `FORGE_AXE_HASHBOARD`
// at build time and takes only the sensors from the hashboard that its
// EEPROM names.
pub fn forge_axe_hashboard<'a>(record: &Record<'a>) -> Result<HashboardDescriptor, BoardError<'a>> {
    let hashboard = match hw_version(record, HardwareType::Hashboard)? {
        "FAHB0001" => ubos_asic::FORGE_AXE_HASHBOARD,
        unknown => return Err(BoardError::HwVersion(unknown)),
    };
    erased(record)?;
    Ok(hashboard)
}

/// The Forge Axe PSU that `record` names.
///
/// # Errors
///
/// Returns a [`BoardError`] when `record` is not of a PSU, names no known
/// PSU, or holds data in its reserved bytes.
pub fn forge_axe_psu<'a>(record: &Record<'a>) -> Result<ForgeAxePsuDescriptor, BoardError<'a>> {
    let psu = match hw_version(record, HardwareType::Psu)? {
        "FAPS0001" => FORGE_AXE_PSU,
        unknown => return Err(BoardError::HwVersion(unknown)),
    };
    erased(record)?;
    Ok(psu)
}

/// The Forge Axe control board that `record` names, with the data that
/// the record holds in the layout that its descriptor names.
///
/// # Errors
///
/// Returns a [`BoardError`] when `record` is not of a control board, names
/// no known control board, or holds data that its layout does not name.
pub fn forge_axe_control_board<'a>(
    record: &Record<'a>,
) -> Result<(ControlBoardDescriptor, Option<ControlBoardEeprom<'a>>), BoardError<'a>> {
    let control_board = match hw_version(record, HardwareType::ControlBoard)? {
        "FACB0001" => CONTROL_BOARD_FORGE_AXE,
        unknown => return Err(BoardError::HwVersion(unknown)),
    };
    let Some(layout) = control_board.eeprom_layout else {
        erased(record)?;
        return Ok((control_board, None));
    };
    let data = layout.parse(record).map_err(BoardError::Reserved)?;
    Ok((control_board, Some(data)))
}

/// The Forge Axe front panel that `record` names.
///
/// # Errors
///
/// Returns a [`BoardError`] when `record` is not of a front panel, names no
/// known front panel, or holds data in its reserved bytes.
pub fn forge_axe_front_panel<'a>(
    record: &Record<'a>,
) -> Result<FrontPanelDescriptor, BoardError<'a>> {
    let front_panel = match hw_version(record, HardwareType::FrontPanel)? {
        "FAFP0001" => FRONT_PANEL_FORGE_AXE,
        unknown => return Err(BoardError::HwVersion(unknown)),
    };
    erased(record)?;
    Ok(front_panel)
}

/// Why the record of a Forge Axe EEPROM names no known board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum BoardError<'a> {
    /// The record names another kind of board than the lookup finds.
    HardwareType(HardwareType),
    /// No board of its kind carries the `hw_version` of the record.
    HwVersion(&'a str),
    /// The reserved bytes of the record hold data that the descriptor of
    /// the board does not name.
    Reserved(forge::Error),
}

/// The `hw_version` of `record`, when it is the record of a board of
/// `hw_type`.
fn hw_version<'a>(record: &Record<'a>, hw_type: HardwareType) -> Result<&'a str, BoardError<'a>> {
    if record.hw_type != hw_type {
        return Err(BoardError::HardwareType(record.hw_type));
    }
    Ok(record.hw_version)
}

/// Check that the reserved bytes of `record` hold no data, as they must on
/// a board whose descriptor names no layout for them.
fn erased<'a>(record: &Record<'a>) -> Result<(), BoardError<'a>> {
    record.check_erased().map_err(BoardError::Reserved)
}

/// The most temperature sensors that a Forge Axe of any of its boards
/// reads.
// NOTE: the lists name every hashboard and PSU that the lookups give.
pub const FORGE_AXE_MAX_SENSORS: usize =
    most_sensors(&[ubos_asic::FORGE_AXE_HASHBOARD.temp]) + most_sensors(&[FORGE_AXE_PSU.temp]);

impl MachineDescriptor<ForgeAxePsuDescriptor> {
    /// Compose the Forge Axe of its control board, hashboard and PSU.
    #[must_use]
    pub const fn forge_axe(
        control_board: ControlBoardDescriptor,
        hashboard: HashboardDescriptor,
        psu: ForgeAxePsuDescriptor,
    ) -> Self {
        Self {
            model: "Forge Axe",
            control_board,
            hashboard,
            chains: FORGE_AXE_CHAINS,
            psu,
            psu_temp: psu.temp,
        }
    }
}

/// The length of the longest of `lists`.
const fn most_sensors(lists: &[&[SensorDescriptor]]) -> usize {
    let mut most = 0;
    let mut i = 0;
    while i < lists.len() {
        if lists[i].len() > most {
            most = lists[i].len();
        }
        i += 1;
    }
    most
}

#[cfg(test)]
#[path = "tests/forge_axe.rs"]
mod tests;
