//! Linux sysfs GPIO backend for `mmm::reset::ResetPin`.

use std::fs::{write, File, OpenOptions};
use std::io::{self, Write};

use mmm::reset::ResetPin;

pub struct SysfsResetPin(File);

impl SysfsResetPin {
    pub fn export(num: u32) -> io::Result<Self> {
        let _ = write("/sys/class/gpio/export", num.to_string());
        write(format!("/sys/class/gpio/gpio{num}/direction"), "out")?;
        let value = OpenOptions::new()
            .write(true)
            .open(format!("/sys/class/gpio/gpio{num}/value"))?;
        Ok(Self(value))
    }
}

impl ResetPin for SysfsResetPin {
    fn enter_reset(&mut self) {
        let _ = self.0.write_all(b"0");
    }
    fn exit_reset(&mut self) {
        let _ = self.0.write_all(b"1");
    }
}
