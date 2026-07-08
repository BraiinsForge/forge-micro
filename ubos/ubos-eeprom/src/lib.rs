//! Hashboard EEPROM support: a bus-agnostic read trait plus the
//! bos-main-compatible wire format for the factory-programmed
//! operational data (product name, serial number, batch, ASIC type,
//! default operating point) every hashboard carries.
//!
//! [`HashboardEeprom`] carries no bus type in its signature so a future
//! chip on a different bus (1-Wire, say) can implement it without the
//! [`format`] parsing layer ever knowing which transport supplied the
//! bytes. [`i2c::I2cEeprom`] is the only concrete implementation today.

#![cfg_attr(not(test), no_std)]
#![allow(async_fn_in_trait)]

use ubos_logging::{Debug2Format, info, warn};

pub mod format;
pub mod i2c;

/// Raw byte-level access to a hashboard EEPROM. One implementation per
/// bus/chip; [`format::HashboardEepromData::parse`] consumes the bytes
/// this produces without needing to know how they were fetched.
pub trait HashboardEeprom {
    type Error;

    /// Read `buf.len()` bytes starting at address 0 into `buf`.
    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;

    /// Read the whole EEPROM once and log the identity it carries.
    ///
    /// A read or parse failure only warns: descriptors stay compile-time
    /// per `docs/MACHINE.md`, so the EEPROM is a traceability and
    /// sanity-check source, not a config source. See `docs/EEPROM.md`.
    /// The numeric fields are logged too, because the CRC validates
    /// bytes, not semantics: a byte-order mistake would parse cleanly
    /// and only a human reading the values catches it.
    async fn log_identity(&mut self)
    where
        Self::Error: core::fmt::Debug,
    {
        let mut raw = [0u8; format::EEPROM_LEN];
        if let Err(e) = self.read(&mut raw).await {
            warn!("hashboard EEPROM read failed: {:?}", Debug2Format(&e));
            return;
        }

        match format::HashboardEepromData::parse(&raw) {
            Ok(data) => {
                info!(
                    "hashboard EEPROM: product={} serial={} asic={} batch={}",
                    data.board_info.product_name,
                    data.board_info.serial_number,
                    data.board_info.asic_type,
                    data.board_info.product_batch,
                );
                info!(
                    "hashboard EEPROM: voltage={}cV freq={}MHz hashrate={}GH power={}W temp={}C fw={}",
                    data.board_info.voltage_centivolts,
                    data.board_info.frequency_mhz,
                    data.board_info.hashrate_gh,
                    data.board_info.power_w,
                    data.board_info.temperature_c,
                    data.firmware_info.version,
                );
            }
            Err(e) => warn!("hashboard EEPROM parse failed: {:?}", Debug2Format(&e)),
        }
    }
}
