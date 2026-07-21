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

//! Linux gpiocdev backend for `ubos_mining::reset::ResetPin`.
//!
//! Resolves the line by name (matching upstream's gpiocdev convention)
//! so the deployment host's gpiochip + offset don't need to be
//! hardcoded.

use std::io;

use gpiocdev::Request;
use gpiocdev::line::Value;
use log::{error, info};

use ubos_mining::reset::ResetPin;

pub const RESET_LINE: &str = "HB0_RESET";

pub struct GpiodResetPin {
    request: Request,
}

impl GpiodResetPin {
    pub fn open(name: &str) -> io::Result<Self> {
        let found = gpiocdev::find_named_line(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gpio line {name} not found"),
            )
        })?;
        let request = Request::builder()
            .with_found_line(&found)
            .as_output(Value::Inactive)
            .request()
            .map_err(io::Error::other)?;
        info!("gpiod: opened {name} as output");
        Ok(Self { request })
    }
}

impl ResetPin for GpiodResetPin {
    async fn enter_reset(&mut self) {
        if let Err(e) = self.request.set_lone_value(Value::Inactive) {
            error!("gpiod enter_reset failed: {e}");
        }
    }
    async fn exit_reset(&mut self) {
        if let Err(e) = self.request.set_lone_value(Value::Active) {
            error!("gpiod exit_reset failed: {e}");
        }
    }
}
