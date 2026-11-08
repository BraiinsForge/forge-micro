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
//! uses to declare its sensor topology. The LM75A, TMP102, TMP461 and
//! TMP464 drivers live in sibling modules, and [`Sensor::read_temperatures`]
//! takes the [`Readings`] of whichever of them a descriptor names.
//! `ubos-thermal` folds those readings, owns the sensor trait and the
//! sample loop, and writes every metric; nothing here decides or
//! publishes anything.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod lm75a;
pub mod tmp102;
pub mod tmp461;
pub mod tmp464;

use embedded_hal_async::i2c::{I2c, SevenBitAddress};
use heapless::Vec;
use ubos_core::units::Temperature;

use crate::lm75a::Lm75a;
use crate::tmp102::Tmp102;
use crate::tmp461::Tmp461;
use crate::tmp464::Tmp464;

/// One sensor of the board's topology, supplied by the board as const
/// data: the part's driver, which carries its I2C address and what each of
/// its channels measures.
#[derive(Clone, Copy)]
pub enum SensorDescriptor {
    Lm75a(Lm75a),
    Tmp102(Tmp102),
    Tmp461(Tmp461),
    Tmp464(Tmp464),
}

impl SensorDescriptor {
    /// The sensor's 7-bit I2C address.
    #[must_use]
    pub const fn address(&self) -> SevenBitAddress {
        match self {
            Self::Lm75a(sensor) => sensor.address,
            Self::Tmp102(sensor) => sensor.address,
            Self::Tmp461(sensor) => sensor.address,
            Self::Tmp464(sensor) => sensor.address,
        }
    }
}

/// One sensor on the board: the descriptor of its part and the bus device
/// it is read through. Each sensor owns its device, so a device that
/// serialises a shared bus, such as embassy's `I2cDevice`, lets every
/// sensor be read on its own.
pub struct Sensor<Bus> {
    descriptor: &'static SensorDescriptor,
    bus: Bus,
}

impl<Bus: I2c> Sensor<Bus> {
    /// The sensor `descriptor` names, read through `bus`.
    #[must_use]
    pub const fn new(descriptor: &'static SensorDescriptor, bus: Bus) -> Self {
        Self { descriptor, bus }
    }

    /// The descriptor of the sensor's part.
    #[must_use]
    pub const fn descriptor(&self) -> &'static SensorDescriptor {
        self.descriptor
    }

    /// Read every channel of the sensor through its driver, which tags
    /// each channel's reading, or the fault that kept it from one, with
    /// what the channel measures.
    pub async fn read_temperatures(&mut self) -> Readings<Bus::Error> {
        match self.descriptor {
            SensorDescriptor::Lm75a(driver) => driver.read_temperatures(&mut self.bus).await,
            SensorDescriptor::Tmp102(driver) => driver.read_temperatures(&mut self.bus).await,
            SensorDescriptor::Tmp461(driver) => driver.read_temperatures(&mut self.bus).await,
            SensorDescriptor::Tmp464(driver) => driver.read_temperatures(&mut self.bus).await,
        }
    }
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
    OutOfRange(Temperature),
    OpenCircuit,
    ShortCircuit,
    Saturated,
}

impl<E> From<lm75a::Error<E>> for SensorFault<E> {
    fn from(e: lm75a::Error<E>) -> Self {
        match e {
            lm75a::Error::I2c(e) => Self::I2c(e),
            lm75a::Error::OutOfRange(temperature) => Self::OutOfRange(temperature),
        }
    }
}

impl<E> From<tmp102::Error<E>> for SensorFault<E> {
    fn from(e: tmp102::Error<E>) -> Self {
        match e {
            tmp102::Error::I2c(e) => Self::I2c(e),
            tmp102::Error::OutOfRange(temperature) => Self::OutOfRange(temperature),
        }
    }
}

impl<E> From<tmp461::Error<E>> for SensorFault<E> {
    fn from(e: tmp461::Error<E>) -> Self {
        match e {
            tmp461::Error::I2c(e) => Self::I2c(e),
            tmp461::Error::OutOfRange(temperature) => Self::OutOfRange(temperature),
            tmp461::Error::OpenCircuit => Self::OpenCircuit,
            tmp461::Error::ShortCircuit => Self::ShortCircuit,
            tmp461::Error::Saturated => Self::Saturated,
        }
    }
}

impl<E> From<tmp464::Error<E>> for SensorFault<E> {
    fn from(e: tmp464::Error<E>) -> Self {
        match e {
            tmp464::Error::I2c(e) => Self::I2c(e),
            tmp464::Error::OutOfRange(temperature) => Self::OutOfRange(temperature),
            tmp464::Error::OpenCircuit => Self::OpenCircuit,
            tmp464::Error::ShortCircuit => Self::ShortCircuit,
        }
    }
}

/// A channel that gave no reading: what the channel measures, and why.
#[derive(Debug, PartialEq, Eq)]
pub struct ReadingError<E> {
    pub source: TempSource,
    pub fault: SensorFault<E>,
}

/// The most readings one sensor yields: a TMP464's local channel and
/// its remote channels.
pub const MAX_READINGS: usize = 5;

/// The outcome of every channel of one sensor: its reading, or the fault
/// that kept it from one. The caller decides what a fault means.
pub type Readings<E> = Vec<Result<Reading, ReadingError<E>>, MAX_READINGS>;
