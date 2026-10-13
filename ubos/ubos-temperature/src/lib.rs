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

//! Board-temperature hardware drivers and the descriptor types a board
//! uses to declare its sensor topology. The LM75A, TMP102 and TMP461
//! drivers live in sibling modules, and [`read_sensor`] takes the
//! [`Readings`] of whichever of them a descriptor names. `ubos-thermal`
//! folds those readings, owns the sensor trait and the sample loop, and
//! writes every metric; nothing here decides or publishes anything.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod lm75a;
pub mod tmp102;
pub mod tmp461;

use embedded_hal_async::i2c::I2c;
use heapless::Vec;
use ubos_core::units::Temperature;
use ubos_logging::error;

use crate::lm75a::Lm75a;
use crate::tmp102::Tmp102;
use crate::tmp461::Tmp461;

/// Which chip driver backs a sensor entry.
#[derive(Clone, Copy)]
pub enum SensorKind {
    Lm75a,
    Tmp102,
    Tmp461,
}

/// One sensor's board topology, supplied by the board as const data.
#[derive(Clone, Copy)]
pub struct SensorDescriptor {
    pub kind: SensorKind,
    pub address: u8,
    /// The chip whose die this sensor's remote diode reads, if any. `Some`
    /// makes the remote reading a [`TempSource::Chip`] one; `None` means a
    /// PCB-surface sensor, read through its local channel alone.
    pub attached_chip: Option<ChipId>,
}

/// Index of a chip on its chain. The chain itself is the reader's to
/// know: a worker reads one chain and names its chips within it.
pub type ChipId = u8;

/// What a [`Reading`] measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempSource {
    /// The hashboard under the sensor.
    Hashboard,
    /// The die of one chip.
    Chip(ChipId),
    /// The air around the board.
    Ambient,
    /// The control board.
    ControlBoard,
    /// The power supply.
    Psu,
}

/// One temperature a sensor measured, and what it measured. Every sensor
/// yields the same shape, whichever driver and channel it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub source: TempSource,
    pub value: Temperature,
}

/// The reading of `source` that `result` holds, or the fault that kept
/// `source` from one.
pub(crate) fn add_source<E, F: Into<SensorFault<E>>>(
    source: TempSource,
    result: Result<Temperature, F>,
) -> Result<Reading, ReadingError<E>> {
    result
        .map(|value| Reading { source, value })
        .map_err(|fault| ReadingError {
            source,
            fault: fault.into(),
        })
}

/// Why a channel gave no reading, normalised across every driver's
/// `Error` enum, which each map in via `From`.
#[derive(Debug, PartialEq, Eq)]
pub enum SensorFault<E> {
    I2c(E),
    OutOfRange(i32),
    OpenCircuit,
    ShortCircuit,
    Saturated,
}

impl<E> From<lm75a::Error<E>> for SensorFault<E> {
    fn from(e: lm75a::Error<E>) -> Self {
        match e {
            lm75a::Error::I2c(e) => Self::I2c(e),
            lm75a::Error::OutOfRange(mc) => Self::OutOfRange(mc),
        }
    }
}

impl<E> From<tmp102::Error<E>> for SensorFault<E> {
    fn from(e: tmp102::Error<E>) -> Self {
        match e {
            tmp102::Error::I2c(e) => Self::I2c(e),
            tmp102::Error::OutOfRange(mc) => Self::OutOfRange(mc),
        }
    }
}

impl<E> From<tmp461::Error<E>> for SensorFault<E> {
    fn from(e: tmp461::Error<E>) -> Self {
        match e {
            tmp461::Error::I2c(e) => Self::I2c(e),
            tmp461::Error::OutOfRange(mc) => Self::OutOfRange(mc),
            tmp461::Error::OpenCircuit => Self::OpenCircuit,
            tmp461::Error::ShortCircuit => Self::ShortCircuit,
            tmp461::Error::Saturated => Self::Saturated,
        }
    }
}

/// A channel that gave no reading: what the channel measures, and why.
#[derive(Debug, PartialEq, Eq)]
pub struct ReadingError<E> {
    pub source: TempSource,
    pub fault: SensorFault<E>,
}

/// Log a fault. `channel` qualifies the out-of-range message for
/// multi-channel sensors and is empty for single-channel ones.
fn log_fault<E: core::fmt::Debug>(chip: &str, address: u8, channel: &str, fault: SensorFault<E>) {
    // The diode faults share one message shape, so they fold to a reason
    // string and log once; I2C and out-of-range have their own shapes.
    let diode = match fault {
        SensorFault::I2c(e) => {
            error!(
                "temperature: {} {:#x} I2C error: {:?}",
                chip,
                address,
                ubos_logging::Debug2Format(&e),
            );
            return;
        }
        SensorFault::OutOfRange(mc) => {
            error!(
                "temperature: {} {:#x}{} reading {} mC out of range",
                chip, address, channel, mc,
            );
            return;
        }
        SensorFault::OpenCircuit => "open-circuit",
        SensorFault::ShortCircuit => "short-circuit",
        SensorFault::Saturated => "floating or railed",
    };
    error!(
        "temperature: {} {:#x} remote diode {}",
        chip, address, diode
    );
}

/// The most readings one sensor yields: a TMP461's local and remote
/// channel.
pub const MAX_READINGS: usize = 2;

/// The readings one sensor yielded. A channel that failed is absent.
pub type Readings = Vec<Reading, MAX_READINGS>;

/// Read one sensor through the driver its kind names, and tag each
/// channel's reading with what it measures: the local channel the
/// hashboard, the remote one the attached chip. A channel that fails is
/// logged and left out.
pub async fn read_sensor<Bus: I2c>(desc: &SensorDescriptor, bus: &mut Bus) -> Readings {
    let address = desc.address;
    match desc.kind {
        SensorKind::Lm75a => add_source(
            TempSource::Hashboard,
            Lm75a::new(address).read_temperature(bus).await,
        )
        .map_err(|error| log_fault("LM75A", address, "", error.fault))
        .ok()
        .into_iter()
        .collect(),
        SensorKind::Tmp102 => add_source(
            TempSource::Hashboard,
            Tmp102::new(address).read_temperature(bus).await,
        )
        .map_err(|error| log_fault("TMP102", address, "", error.fault))
        .ok()
        .into_iter()
        .collect(),
        SensorKind::Tmp461 => {
            let sensor = Tmp461::new(address);
            let hashboard = add_source(
                TempSource::Hashboard,
                sensor.read_local_temperature(bus).await,
            )
            .map_err(|error| log_fault("TMP461", address, " local", error.fault))
            .ok();
            let chip = match desc.attached_chip {
                Some(chip) => add_source(
                    TempSource::Chip(chip),
                    sensor.read_remote_temperature(bus).await,
                )
                .map_err(|error| log_fault("TMP461", address, " remote", error.fault))
                .ok(),
                None => None,
            };
            [hashboard, chip].into_iter().flatten().collect()
        }
    }
}
