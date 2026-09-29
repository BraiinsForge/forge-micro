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

//! hwmon-based fan controller. Drives PID-modulated PWM via
//! `ubos_cooling::run_cooling` fed by the BMM101 LM75A array.
//!
//! Ported from the BOS hwmon fan controller. The kernel
//! `pwm-fan` driver wraps a raw PWM with an hwmon device, so there is
//! no `pwmchipN/export` dance: each fan exposes a single 0..255
//! `pwm1` attribute and the controller just walks
//! `/sys/devices/platform/` for entries whose `name` file contains
//! `pwmfan`. RPM feedback (`fan{1..9}_input`) from the upstream is
//! omitted.

use std::fs;
use std::path::{Path, PathBuf};

use log::{error, info, warn};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};
use ubos_thermal::Handle as ThermalHandle;

/// Root of the platform-device tree the kernel exposes for hwmon
/// drivers. Hardcoded — same as the upstream constant.
const PLATFORM_DEV_PATH: &str = "/sys/devices/platform";

/// Device-tree binding name the `pwm-fan` driver registers under.
const PWMFAN_DRIVER_NAME: &str = "pwmfan";

/// PWM input file every `pwm-fan` instance exposes. Always `pwm1`
/// because each `pwm-fan` instance drives a single PWM and hwmon
/// numbers PWM/fan attributes from 1.
const PWM_FILE: &str = "pwm1";

/// Bounded-depth recursive search ceiling. `pwmfan` typically appears
/// at depth 1 or 2 under `/sys/devices/platform/`; six is generous
/// without ever risking a runaway walk on an unfamiliar tree.
const MAX_SEARCH_DEPTH: usize = 6;

/// Full-scale value of the hwmon PWM attribute.
const MAX_PWM_VALUE: u8 = 255;

/// Floor below which `set_speed` refuses to go. The kernel's
/// `pwm-fan` RPM sampling diverges hard near zero, and on inverted
/// PWM hardware writing `0` is interpreted as "full speed" rather
/// than "stopped". Keeping a small base PWM sidesteps both. Lifted
/// straight from the upstream comment.
const MIN_PWM_VALUE: u8 = 15;

/// Owns one writable `pwm1` path per detected fan.
pub struct FanHwmonController {
    pwm_paths: Vec<PathBuf>,
}

impl FanHwmonController {
    /// Walk `/sys/devices/platform/` for `pwm-fan` instances and
    /// collect their `pwm1` paths. Returns `None` if the platform
    /// tree is missing (e.g. dev-host TAP build) or no `pwmfan`
    /// device is present — caller decides whether to spawn the task.
    pub fn open() -> Option<Self> {
        let root = Path::new(PLATFORM_DEV_PATH);
        if !root.exists() {
            warn!("fan: {PLATFORM_DEV_PATH} not present; fan control disabled");
            return None;
        }

        let device_dirs = find_pwmfan_devices(root, 0);

        let pwm_paths: Vec<PathBuf> = device_dirs
            .iter()
            .map(|dir| dir.join(PWM_FILE))
            .filter(|path| {
                let exists = path.exists();
                if !exists {
                    warn!("fan: {} missing; skipping", path.display());
                }
                exists
            })
            .collect();

        if pwm_paths.is_empty() {
            warn!("fan: no pwmfan devices found under {PLATFORM_DEV_PATH}; fan control disabled");
            return None;
        }

        info!("fan: opened {} pwmfan device(s)", pwm_paths.len());
        Some(Self { pwm_paths })
    }

    /// Write 100% to every detected fan. Called as a safe-high
    /// fallback when no temperature source is available, so the
    /// chassis cools rather than coasts on the kernel default.
    pub fn park_at_full_speed(&self) {
        self.set_speed(100);
    }

    fn set_speed(&self, percent: u8) {
        // Map 0..100 to 0..255 with the upstream MIN_PWM_VALUE floor.
        let percent = u32::from(percent.min(100));
        let scaled = (u32::from(MAX_PWM_VALUE) * percent + 50) / 100;
        let pwm_value = u8::try_from(scaled)
            .unwrap_or(MAX_PWM_VALUE)
            .max(MIN_PWM_VALUE);

        for path in &self.pwm_paths {
            if let Err(error) = fs::write(path, pwm_value.to_string()) {
                error!(
                    "fan: failed to write {pwm_value} to {}: {error}",
                    path.display()
                );
            }
        }
    }
}

/// Recursive walker mirroring `SysFsPath::enumerate_platform_devices`
/// from the upstream `ii-sysfs` crate, sized down to `std::fs` so we
/// don't take a dependency on `walkdir` for one call site. Returns the
/// parent directory of every `name` file whose contents trim to
/// [`PWMFAN_DRIVER_NAME`].
fn find_pwmfan_devices(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut devices = Vec::new();
    if depth > MAX_SEARCH_DEPTH {
        return devices;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return devices;
    };
    for entry in entries.flatten() {
        // sysfs sprays symlinks (subsystem, driver, ...) that loop back
        // up the tree; following them would diverge the walk.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name_file = path.join("name");
        if let Ok(content) = fs::read_to_string(&name_file)
            && content.trim() == PWMFAN_DRIVER_NAME
        {
            devices.push(path.clone());
        }
        devices.extend(find_pwmfan_devices(&path, depth + 1));
    }
    devices
}

impl FanOutput for FanHwmonController {
    fn set_pwm(&mut self, pwm: FanPwm) {
        self.set_speed(pwm.percent());
    }
}

/// Tachometer stub. The host runner omits RPM feedback (see module
/// doc), so it reports zero and the cooling loop publishes PWM only.
struct NoTach;

impl TachReader<0> for NoTach {
    const LABELS: [Option<&'static str>; 0] = [];

    async fn read_rpms(&mut self) -> [FanRpm; 0] {
        []
    }
}

/// PID-driven fan loop. Spawned by main once both the fan controller
/// and the LM75A sensor array have opened.
#[embassy_executor::task]
pub async fn pid_fan_task(controller: FanHwmonController, thermal_handle: ThermalHandle) -> ! {
    ubos_cooling::run_cooling(controller, NoTach, thermal_handle, ubos_config::handle()).await
}
