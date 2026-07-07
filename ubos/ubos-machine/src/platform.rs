//! Control-board MCU platforms.

/// The MCU a control board is built around.
///
/// Identity and reporting metadata only. The concrete HAL, target, and
/// memory layout are selected by the firmware binary crate, not by this
/// data, because they are compile-time choices the type system fixes.
/// Only `name` has a consumer today; `core` and `target` are
/// scaffolding for the planned build and reporting work.
#[derive(Clone, Copy)]
pub struct PlatformDescriptor {
    /// Short platform name. Matches the firmware crate directory for
    /// every platform except `hashnode`, which reuses the `RP2350`
    /// platform const and so reports `"rp2350"`.
    pub name: &'static str,
    /// CPU core family, e.g. `cortex-m33`.
    pub core: &'static str,
    /// Rust target triple the firmware for this platform builds against.
    pub target: &'static str,
}

/// RP2040 platform: Cortex-M0+.
pub const RP2040: PlatformDescriptor = PlatformDescriptor {
    name: "rp2040",
    core: "cortex-m0plus",
    target: "thumbv6m-none-eabi",
};

/// RP2350A platform: Cortex-M33. Shared by the rp2350 and bmm-pico
/// control boards.
pub const RP2350: PlatformDescriptor = PlatformDescriptor {
    name: "rp2350",
    core: "cortex-m33",
    target: "thumbv8m.main-none-eabihf",
};

/// STM32F407VGT6 platform: Cortex-M4F.
pub const STM32F407: PlatformDescriptor = PlatformDescriptor {
    name: "stm32f407",
    core: "cortex-m4f",
    target: "thumbv7em-none-eabihf",
};

/// STM32F439ZIT6 platform: Cortex-M4F.
pub const STM32F439: PlatformDescriptor = PlatformDescriptor {
    name: "stm32f439",
    core: "cortex-m4f",
    target: "thumbv7em-none-eabihf",
};

/// ESP32-S3 platform: Xtensa LX7.
pub const ESP32S3: PlatformDescriptor = PlatformDescriptor {
    name: "esp32s3",
    core: "xtensa-lx7",
    target: "xtensa-esp32s3-none-elf",
};
