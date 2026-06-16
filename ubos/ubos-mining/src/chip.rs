//! Chip-level operations that compose typed register reads and writes.
//!
//! Each function expresses one semantic chip operation using the typed
//! register structs from `ubos-asic` and the
//! `crate::serial::SERIAL` actor. This module is the only caller of
//! the typed `write_register` / `read_register` paths; nothing outside
//! `ubos-mining` may import it.

use crate::miner::MinerError;
use crate::serial::SERIAL;
use ubos_asic::{
    BM1362, ChipIdReg, HashCountingNumberReg, Pll0DividerReg, PllParameterReg, RawChipAddress,
    Register, TicketMaskReg,
};

/// Program a frequency change on `chip` using the captured wire
/// trace's per-step pattern: `HCN → Pll0Divider=0 → PllParameter →
/// Pll0Divider=0 → PllParameter`. The trailing duplicate covers the
/// case where the first PLL write arrives garbled because the PLL
/// itself shifted mid-reception. `target_khz` granularity matches
/// a sub-MHz frequency ramp without truncation.
pub async fn set_frequency(chip: RawChipAddress, target_khz: u32) -> Result<(), MinerError> {
    let range = BM1362.frequency_range;
    let pll = PllParameterReg::from_freq(target_khz, range)
        .ok_or(MinerError::UnsupportedFrequency(target_khz))?;
    let hcn = HashCountingNumberReg::from_freq(target_khz, range)
        .ok_or(MinerError::UnsupportedFrequency(target_khz))?;
    let divider_fence = Pll0DividerReg::zeroed();
    SERIAL.write_register(chip, &hcn).await;
    SERIAL.write_register(chip, &divider_fence).await;
    SERIAL.write_register(chip, &pll).await;
    SERIAL.write_register(chip, &divider_fence).await;
    SERIAL.write_register(chip, &pll).await;
    Ok(())
}

/// Write the ticket-mask register to all chips. The raw mask value is
/// the wire encoding; use `TicketMaskReg::from_difficulty` at the call
/// site to derive it from a share difficulty.
pub async fn set_ticket_mask(mask: u32) -> Result<(), MinerError> {
    let reg = TicketMaskReg::new().with_ticket_mask(mask);
    SERIAL.write_register(RawChipAddress::All, &reg).await;
    Ok(())
}

/// Read the chip-id register from a single chip. Returns `TimedOut`
/// wrapped as `MinerError::Serial` when the chip does not respond
/// within the actor's reply window.
pub async fn get_chip_version(chip: u8) -> Result<ChipIdReg, MinerError> {
    SERIAL
        .read_register::<ChipIdReg>(chip)
        .await
        .map_err(MinerError::Serial)
}

/// Broadcast-read `ChipIdReg` and return the number of responding
/// chips. The serial actor caps broadcast reads at `NUM_CHIPS` reply
/// slots, so the count reflects responses observed within that
/// window rather than probing the true chain length.
pub async fn enumerate_chips() -> Result<u8, MinerError> {
    let responses = SERIAL
        .read_register_raw(RawChipAddress::All, ChipIdReg::REG_NUM)
        .await;
    // Truncating cast: response count is bounded by the `NUM_CHIPS` reply slots above.
    #[allow(clippy::cast_possible_truncation)]
    Ok(responses.len() as u8)
}

#[cfg(test)]
#[path = "tests/chip.rs"]
mod tests;
