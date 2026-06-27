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
    fn enter_reset(&mut self) {
        if let Err(e) = self.request.set_lone_value(Value::Inactive) {
            error!("gpiod enter_reset failed: {e}");
        }
    }
    fn exit_reset(&mut self) {
        if let Err(e) = self.request.set_lone_value(Value::Active) {
            error!("gpiod exit_reset failed: {e}");
        }
    }
}
