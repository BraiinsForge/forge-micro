//! `ChipBus` implementation over the serial actor.
//!
//! Bridges the pure `ubos_asic::ChipProtocol` bring-up recipes to the
//! real chip serial link: register and command traffic forwards to the
//! [`SERIAL`] actor, and delays are backed by embassy-time. This is the
//! consumer side of the sans-IO seam (see `.claude/rules/rust.md`).

use embassy_time::{Duration, Timer};
use heapless::Vec;
use ubos_asic::{ChipAddress, ChipBus, ControlCommand, MAX_CHAIN_CHIPS, Register, build_command};

use crate::serial::SERIAL;

/// `ChipBus` over the workspace serial actor. Zero-sized: it reaches the
/// single `SERIAL` static directly, matching how the rest of
/// `ubos-mining` consumes the actor.
pub struct SerialBus;

impl ChipBus for SerialBus {
    async fn write_register_raw(&mut self, addr: ChipAddress, reg: u8, value: u32) {
        SERIAL.write_register_raw(addr, reg, value).await;
    }

    async fn read_register<R: Register>(
        &mut self,
        chip_address: ChipAddress,
    ) -> Vec<(u8, R), MAX_CHAIN_CHIPS> {
        // Broadcast reads discover the chain; a single chip read expects
        // one reply. The actor filters out mismatched-register replies.
        let replies = SERIAL.read_register(chip_address, R::REG_NUM).await;
        let mut responders = Vec::new();
        for reply in replies {
            // Both buffers are sized to MAX_CHAIN_CHIPS, so this never
            // overflows; stop rather than panic if that ever changes.
            if responders
                .push((reply.chip_addr, R::from_be_bytes(reply.value.to_be_bytes())))
                .is_err()
            {
                break;
            }
        }
        responders
    }

    async fn send_command(&mut self, command: ControlCommand) {
        SERIAL.send_frame(build_command(command)).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        Timer::after(Duration::from_millis(u64::from(ms))).await;
    }
}
