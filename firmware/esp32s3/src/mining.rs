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

//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the ubos-mining backend needs.
//!
//! `serial_task` owns the UART and drives `ubos_mining::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `ubos_mining::Miner::run`. Neither ever returns under normal
//! operation.
//!
//! The ubos-mining `serial_task` runs on Core 1 (the `APP_CPU` embassy executor)
//! while embassy-net and the mining state machine run on Core 0, mirroring
//! the rp2350/bmm-pico dual-core split. `ASIC_SESSION` uses a
//! `CriticalSectionRawMutex`, so the cross-core share needs no extra pipe.

use defmt::{Debug2Format, error, info, warn};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Duration, Timer};
use esp_hal::i2c::master::I2c;
use tca9554::{Address, Tca9554};

use crate::psu::{BoardPsu, Bus};
use crate::uart_baudrate::MinerUart;

/// `embedded-hal-async` handle for one consumer of the shared I2C0 bus.
type ExpanderDevice = I2cDevice<'static, NoopRawMutex, I2c<'static, esp_hal::Async>>;

/// EXIO5 (P4) on the carrier's TCA9554 drives the hashboard reset line.
/// The EXIO labels are 1-based on the carrier silkscreen while the TCA9554
/// pins are 0-based, so EXIO5 maps to pin P4. Confirm this offset against
/// the carrier schematic if the expander wiring ever changes.
const RESET_BIT: u8 = 1 << 4;

/// Drives the hashboard reset line through the carrier's TCA9554 I/O
/// expander (0x20) on EXIO5. Active-low: low holds the chain in reset,
/// high releases it.
pub struct BoardResetPin {
    expander: Tca9554<ExpanderDevice>,
}

impl BoardResetPin {
    /// Carve an expander handle from the shared I2C0 bus, configure EXIO5
    /// as an output and assert reset (drive low) so the miner task
    /// controls release timing. A failed expander write is logged and
    /// ignored, per [`ubos_mining::reset::ResetPin`].
    pub async fn new(bus: &'static Bus) -> Self {
        let mut expander = Tca9554::new(I2cDevice::new(bus), Address::standard());
        // Drive EXIO5 low before switching it to an output. The TCA9554 output
        // latch powers up at 0xFF, so writing the direction first would briefly
        // drive EXIO5 high and deassert reset before the output write lands.
        // Output register: 0 = low on every pin. Direction register: 0 = output.
        if expander.write_output(0).await.is_err()
            || expander.write_direction(!RESET_BIT).await.is_err()
        {
            warn!("expander: TCA9554 0x20 reset-pin init failed");
        }
        Self { expander }
    }
}

impl ubos_mining::reset::ResetPin for BoardResetPin {
    async fn enter_reset(&mut self) {
        if self.expander.write_output(0).await.is_err() {
            warn!("expander: TCA9554 0x20 enter_reset write failed");
        }
    }

    async fn exit_reset(&mut self) {
        if self.expander.write_output(RESET_BIT).await.is_err() {
            warn!("expander: TCA9554 0x20 exit_reset write failed");
        }
    }
}

/// Long park used after either task's main loop returns. The ubos-mining actor
/// only returns on a fatal serial error; the task idles so the rest of the
/// system keeps running and a human can pull logs.
const PARK_INTERVAL: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn serial_task(
    serial: MinerUart,
    session: &'static ubos_firmware::AsicSession<{ crate::POOL_SLOTS }>,
) -> ! {
    match ubos_mining::serial::SERIAL
        .run(
            serial,
            session,
            crate::config::BOARD,
            ubos_asic::Bm13xxDriver::new(crate::config::CHIP),
            ubos_mining::scheduler::RatioScheduler::new(),
        )
        .await
    {
        Ok(()) => info!("ubos-mining serial actor terminated cleanly"),
        Err(kind) => error!("ubos-mining serial actor exited: {:?}", Debug2Format(&kind)),
    }
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}

#[embassy_executor::task]
pub async fn miner_task(psu: &'static BoardPsu, reset: BoardResetPin) -> ! {
    let mut miner = ubos_mining::Miner::new(psu, reset, crate::config::BOARD);
    miner.run().await;
    info!("ubos-mining miner exited");
    loop {
        Timer::after(PARK_INTERVAL).await;
    }
}
