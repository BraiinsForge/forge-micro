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
//! drivers live in sibling modules, and [`read_sensor`] takes one
//! reading from whichever of them a descriptor names. `ubos-thermal`
//! folds those readings, owns the sensor trait and the sample loop, and
//! writes every metric; nothing here decides or publishes anything.

#![no_std]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod lm75a;
pub mod tmp102;
pub mod tmp461;

use embedded_hal_async::i2c::I2c;
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
    /// Added to a PCB sensor's reading to approximate die temperature.
    pub pcb_to_die_offset_mc: i32,
    /// Chip index whose remote diode this sensor reads, if any. `Some`
    /// marks the sensor's remote reading as a trustworthy die temperature;
    /// `None` means a PCB-surface sensor. A plain index for now — nothing
    /// consumes the value yet.
    pub attached_chip: Option<u8>,
}

/// A sensor's local (own body/PCB) and optional remote (die diode)
/// readings in millicelsius.
pub struct DualReading {
    pub local: Option<i32>,
    pub remote: Option<i32>,
}

/// A sensor fault normalized across every driver's `Error` enum, so all
/// fault logging lives in one place. Each driver's error maps in via
/// `From`; the drivers themselves stay pure `Result`-returning wrappers.
enum SensorFault<E> {
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

/// Take one channel reading: pass the value through, or log the fault and
/// yield `None`.
fn take<E: core::fmt::Debug>(
    chip: &str,
    address: u8,
    channel: &str,
    reading: Result<i32, SensorFault<E>>,
) -> Option<i32> {
    match reading {
        Ok(mc) => Some(mc),
        Err(fault) => {
            log_fault(chip, address, channel, fault);
            None
        }
    }
}

/// Read one sensor, dispatching on kind to name its concrete driver type
/// and channels. All fault logging is funneled through `take`.
pub async fn read_sensor<Bus: I2c>(desc: &SensorDescriptor, bus: &mut Bus) -> DualReading {
    let addr = desc.address;
    match desc.kind {
        SensorKind::Lm75a => DualReading {
            local: take(
                "LM75A",
                addr,
                "",
                Lm75a::new(addr)
                    .read_millicelsius(bus)
                    .await
                    .map_err(SensorFault::from),
            ),
            remote: None,
        },
        SensorKind::Tmp102 => DualReading {
            local: take(
                "TMP102",
                addr,
                "",
                Tmp102::new(addr)
                    .read_millicelsius(bus)
                    .await
                    .map_err(SensorFault::from),
            ),
            remote: None,
        },
        SensorKind::Tmp461 => {
            let sensor = Tmp461::new(addr);
            DualReading {
                remote: take(
                    "TMP461",
                    addr,
                    " remote",
                    sensor
                        .read_remote_millicelsius(bus)
                        .await
                        .map_err(SensorFault::from),
                ),
                local: take(
                    "TMP461",
                    addr,
                    " local",
                    sensor
                        .read_local_millicelsius(bus)
                        .await
                        .map_err(SensorFault::from),
                ),
            }
        }
    }
}
