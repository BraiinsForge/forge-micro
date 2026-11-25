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

//! hwmon-based fan controller feeding `ubos_cooling::run_cooling`.
//!
//! The kernel `pwm-fan` driver wraps a raw PWM in an hwmon device, so a
//! fan is found by walking `/sys/devices/platform/` for a `name` of
//! `pwmfan` and driven through that device's single `pwm1` attribute.
//! RPM feedback is not read here.

use std::fs;
use std::path::{Path, PathBuf};

use log::{error, info, warn};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};
use ubos_thermal::Handle as ThermalHandle;

const PLATFORM_DEV_PATH: &str = "/sys/devices/platform";

/// Device-tree binding name the `pwm-fan` driver registers under.
const PWMFAN_DRIVER_NAME: &str = "pwmfan";

/// Always `pwm1`, because a `pwm-fan` instance drives one PWM and hwmon
/// numbers its attributes from 1.
const PWM_FILE: &str = "pwm1";

/// A `pwmfan` device sits at depth 1 or 2 under the platform tree, so
/// this leaves headroom without risking a runaway walk.
const MAX_SEARCH_DEPTH: usize = 6;

/// Full-scale value of the hwmon PWM attribute.
const MAX_PWM_VALUE: u8 = 255;

/// On inverted PWM hardware a written `0` means full speed rather than
/// stopped, and the kernel's `pwm-fan` RPM sampling diverges near zero,
/// so `set_speed` holds a small base PWM instead.
const MIN_PWM_VALUE: u8 = 15;

pub struct FanHwmonController {
    pwm_paths: Vec<PathBuf>,
}

impl FanHwmonController {
    /// Returns `None` when the platform tree is absent or carries no
    /// `pwmfan` device, leaving the caller to decide whether to spawn
    /// the cooling task.
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

    /// The safe-high fallback when no temperature source opened, so the
    /// chassis cools rather than coasting on the kernel default.
    pub fn park_at_full_speed(&self) {
        self.set_speed(100);
    }

    fn set_speed(&self, percent: u8) {
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

/// Returns the parent directory of every `name` file whose contents trim
/// to [`PWMFAN_DRIVER_NAME`].
fn find_pwmfan_devices(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut devices = Vec::new();
    if depth > MAX_SEARCH_DEPTH {
        return devices;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return devices;
    };
    for entry in entries.flatten() {
        // sysfs symlinks loop back up the tree, so following one would
        // diverge the walk.
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
    async fn set_pwm(&mut self, pwm: FanPwm) {
        self.set_speed(pwm.percent());
    }
}

/// Reports no fans, because this runner reads no RPM feedback. See the
/// module doc.
struct NoTach;

impl TachReader<0> for NoTach {
    const LABELS: [Option<&'static str>; 0] = [];

    async fn read_rpms(&mut self) -> [FanRpm; 0] {
        []
    }
}

#[embassy_executor::task]
pub async fn pid_fan_task(
    controller: FanHwmonController,
    mut cooling_runner: ubos_cooling::Runner,
    thermal_handle: ThermalHandle,
    config_handle: ubos_config::Handle,
) -> ! {
    ubos_cooling::run_cooling(
        controller,
        NoTach,
        &mut cooling_runner,
        thermal_handle,
        config_handle,
    )
    .await
}
