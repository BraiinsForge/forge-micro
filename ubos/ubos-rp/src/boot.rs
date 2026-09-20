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

//! The boot glue every RP board repeats: the chip id the MAC derives from,
//! the entropy seeds and the `WIZnet` bring-up with its failure policy.
//!
//! These are free functions: each reaches a hardware singleton through a
//! peripheral token or a chip register, or drives the network device and
//! its runner state as peers, so none has a receiver this crate declares.

use defmt::error;
use embassy_net_wiznet::chip::Chip;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::OutputPin;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;

/// How long the board waits before it resets after a failed `WIZnet` init,
/// so the log line reaches the probe first.
const WIZNET_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The RP2350 OTP chip id as big-endian bytes, or zero when the read
/// fails, so the MAC derived from it is stable rather than absent.
#[cfg(feature = "rp235x")]
#[must_use]
pub fn chip_id() -> [u8; 8] {
    embassy_rp::otp::get_chipid()
        .unwrap_or_else(|_| {
            defmt::warn!("OTP chip id read failed; MAC hash falls back to zero");
            0
        })
        .to_be_bytes()
}

/// The RP2040 flash unique id, or zero when the read fails, so the MAC
/// derived from it is stable rather than absent. The id is an inherent
/// method on `Flash` that the `NorFlash` traits do not expose, so a board
/// reads it before the driver moves into its shared mutex.
#[cfg(not(feature = "rp235x"))]
#[must_use]
pub fn flash_unique_id<T, M, const SIZE: usize>(
    flash: &mut embassy_rp::flash::Flash<'_, T, M, SIZE>,
) -> [u8; 8]
where
    T: embassy_rp::flash::Instance,
    M: embassy_rp::flash::Mode,
{
    let mut uid = [0u8; 8];
    if flash.blocking_unique_id(&mut uid).is_err() {
        defmt::warn!("flash unique id read failed; MAC hash falls back to zero");
        return [0u8; 8];
    }
    uid
}

/// The three seeds a board draws at boot.
pub struct Seeds {
    /// The embassy-net stack seed.
    pub net: u64,
    /// The session CSPRNG seed.
    pub csprng: [u8; 32],
    /// The SV2 noise handshake seed.
    pub sv2: [u8; 32],
}

/// Every seed from the ring oscillator. The RP2040 has no hardware TRNG,
/// so `RoscRng` is its only source: one ROSC random bit per read with no
/// health check.
#[must_use]
pub fn seeds_from_rosc() -> Seeds {
    let mut rng = RoscRng;
    let mut csprng = [0u8; 32];
    let mut sv2 = [0u8; 32];
    rng.fill_bytes(&mut csprng);
    rng.fill_bytes(&mut sv2);
    Seeds {
        net: rng.next_u64(),
        csprng,
        sv2,
    }
}

/// TRNG sample count per draw. Counts of 20 to 25 minimise generation
/// time but fail the on-die entropy checks often enough to log every
/// boot; 100 trades a few hundred microseconds for a quiet boot.
#[cfg(feature = "rp235x")]
const TRNG_SAMPLE_COUNT: u32 = 100;

/// The session and handshake seeds from the hardware TRNG, the stack seed
/// from the ring oscillator. The TRNG runs three built-in entropy health
/// checks per draw; it is used at boot only, so the peripheral is dropped
/// once the seeds are drawn.
#[cfg(feature = "rp235x")]
pub async fn seeds_from_trng<T: embassy_rp::trng::Instance>(
    trng: embassy_rp::Peri<'_, T>,
    irq: impl embassy_rp::interrupt::typelevel::Binding<
        T::Interrupt,
        embassy_rp::trng::InterruptHandler<T>,
    >,
) -> Seeds {
    let mut config = embassy_rp::trng::Config::default();
    config.sample_count = TRNG_SAMPLE_COUNT;
    let mut trng = embassy_rp::trng::Trng::new(trng, irq, config);
    let mut csprng = [0u8; 32];
    let mut sv2 = [0u8; 32];
    trng.fill_bytes(&mut csprng).await;
    trng.fill_bytes(&mut sv2).await;
    Seeds {
        net: RoscRng.next_u64(),
        csprng,
        sv2,
    }
}

/// Bring the `WIZnet` chip up, or reset the board and try again.
///
/// A failed SPI read is almost always transient, a brownout garbling the
/// bus rather than a wiring fault. A chip-version mismatch is not
/// transient: it means the wrong driver type or a dead chip, and no reset
/// clears it. The reset loop is still what runs, because a headless board
/// has no channel to report on before its network is up, and the driver
/// logs both version bytes over the probe. `new` consumes the SPI, both
/// pins and the init-once `State`, so it cannot be retried in place, and a
/// panic here records a crash marker the OTA path may act on for what is
/// not a firmware fault. A dead chip loops through this reset, the same
/// standstill a panic gave, without the false marker and without waiting
/// out the watchdog.
pub async fn wiznet_or_reset<
    const N_RX: usize,
    const N_TX: usize,
    C: Chip,
    SPI: SpiDevice,
    INT: Wait,
    RST: OutputPin,
>(
    mac: [u8; 6],
    state: &mut State<N_RX, N_TX>,
    spi: SPI,
    int: INT,
    reset: RST,
) -> (Device<'_>, Runner<'_, C, SPI, INT, RST>) {
    let Ok(pair) = embassy_net_wiznet::new(mac, state, spi, int, reset).await else {
        error!("WIZnet init failed; resetting to retry");
        Timer::after(WIZNET_RETRY_DELAY).await;
        cortex_m::peripheral::SCB::sys_reset();
    };
    pair
}
