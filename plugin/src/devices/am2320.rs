//! AM2320 temperature/humidity sensor -- a teaching-sample environmental
//! sensor, the plain-register-extraction counterpart to
//! [`crate::devices::bmp280::Bmp280`]/[`crate::devices::bme680::Bme680`]'s
//! Bosch-compensation-formula devices (those two need real
//! calibration-coefficient math; this one doesn't).
//!
//! Wire protocol (Aosong's own, a cut-down single-byte-address Modbus
//! variant -- confirmed against Henryk Richter's `i2csensors` driver,
//! https://gitlab.com/HenrykRichter/i2csensors,
//! `sensors/src/i2cclass_sensor.c`'s `wakeup`/`readpre` handling and
//! `sensors/devs/Sensors/AM2320.cfg`'s exact byte sequences, since the
//! chip has no address-select pins to make config-driven addressing
//! meaningful and its wake-then-command shape isn't a flat register
//! pointer like this project's other devices):
//! 1. **Wake**: a 1-byte master-receive, then a 0-byte master-transmit,
//!    both to this device's address, neither's result checked by the
//!    driver (a real AM2320 NAKs both while asleep; this emulation just
//!    ACKs everything, which is equally harmless either way).
//! 2. **Command**: a 3-byte master-transmit, `[0x03, start_addr, count]`
//!    -- function code 3 ("read registers"), and this chip only has two
//!    2-byte registers, humidity at 0x00 and temperature at 0x02, so
//!    `start_addr` alone (no separate humidity/temperature command
//!    byte) is what picks which value the next read returns.
//! 3. **Response**: a 6-byte master-receive, `[0x03, 0x04, data_hi,
//!    data_lo, crc_lo, crc_hi]` on a real chip -- the driver never reads
//!    or checks the CRC bytes, so this emulation doesn't bother
//!    computing them either, just emits zero.
//!
//! Temperature is sign-magnitude, not two's complement (a real
//! quirk of this chip, unlike every other device on this bus): bit 15
//! of the 16-bit value is a plain negative flag, bits 14-0 are the
//! magnitude. Humidity is plain unsigned. Both are in units of 0.1
//! (°C or %RH) -- confirmed against AM2320.cfg's `MUL = 0.1` and its
//! `BITOFFSET`/`SIGNBIT`/`NUMBITS` values (which count bits from the
//! *start* of the read buffer, same convention this project's
//! `examples/Sensors/LM75.cfg` had to work out empirically).
//!
//! Fixed 7-bit address 0x5C (0xB8 8-bit, AM2320.cfg's own
//! `I2CADDRESS`) -- no address-select pins, same treatment as
//! [`crate::devices::ds1307::DS1307_ADDRESS`].

use crate::i2c::I2cDevice;

/// Fixed I2C address of every real AM2320 -- no address pins to strap.
pub const AM2320_ADDRESS: u8 = 0x5C;

const REG_HUMIDITY: u8 = 0x00;
const REG_TEMPERATURE: u8 = 0x02;

pub struct Am2320 {
    celsius: f32,
    humidity_percent: f32,
    /// Bytes of the in-progress command write (`start()` for a write
    /// clears this); once it holds exactly 3 bytes, byte 1 (the start
    /// address) selects which register the next read responds with.
    command: [u8; 3],
    command_len: u8,
    selected: u8,
    /// Position within the current 6-byte (or 1-byte, for the wake
    /// dummy-read) response burst.
    read_pos: u8,
}

impl Am2320 {
    pub fn new() -> Self {
        Self {
            celsius: 25.0,
            humidity_percent: 50.0,
            command: [0; 3],
            command_len: 0,
            selected: REG_HUMIDITY,
            read_pos: 0,
        }
    }

    /// Scenario/test hook: set the sensed temperature.
    pub fn set_celsius(&mut self, celsius: f32) {
        self.celsius = celsius;
    }

    /// Scenario/test hook: set the sensed relative humidity, 0-100.
    pub fn set_humidity_percent(&mut self, percent: f32) {
        self.humidity_percent = percent;
    }

    fn selected_word(&self) -> u16 {
        if self.selected == REG_TEMPERATURE {
            let magnitude = ((self.celsius.abs() * 10.0).round() as u16).min(0x7FFF);
            if self.celsius < 0.0 {
                magnitude | 0x8000
            } else {
                magnitude
            }
        } else {
            (self.humidity_percent * 10.0).round() as u16
        }
    }

    fn response_byte(&self, pos: u8) -> u8 {
        match pos {
            0 => 0x03,
            1 => 0x04,
            2 => (self.selected_word() >> 8) as u8,
            3 => self.selected_word() as u8,
            _ => 0x00, // CRC bytes on a real chip -- never read by this driver
        }
    }
}

impl Default for Am2320 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Am2320 {
    fn start(&mut self, read: bool) -> bool {
        if read {
            self.read_pos = 0;
        } else {
            self.command_len = 0;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if (self.command_len as usize) < self.command.len() {
            self.command[self.command_len as usize] = byte;
            self.command_len += 1;
            if self.command_len == 3 {
                self.selected = self.command[1];
            }
        }
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        // Both the wake dummy-read (1 byte, content ignored by the
        // driver) and the real 6-byte response are served the same way
        // -- `response_byte` just keeps returning 0x00 past index 3,
        // which is exactly right for the wake read too.
        let byte = self.response_byte(self.read_pos);
        self.read_pos = self.read_pos.wrapping_add(1);
        byte
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_command(dev: &mut Am2320, start_addr: u8, count: u8) -> u16 {
        dev.start(false);
        dev.write(0x03);
        dev.write(start_addr);
        dev.write(count);
        dev.start(true);
        let _func = dev.read(true);
        let _byte_count = dev.read(true);
        (u16::from(dev.read(true)) << 8) | u16::from(dev.read(true))
    }

    #[test]
    fn defaults_to_room_temperature_and_fifty_percent_humidity() {
        let mut dev = Am2320::new();
        assert_eq!(read_command(&mut dev, REG_TEMPERATURE, 2), 250); // 25.0C -> 250 (0.1C units)
        assert_eq!(read_command(&mut dev, REG_HUMIDITY, 2), 500); // 50.0% -> 500 (0.1% units)
    }

    #[test]
    fn negative_temperature_is_sign_magnitude_not_twos_complement() {
        let mut dev = Am2320::new();
        dev.set_celsius(-5.3);
        let word = read_command(&mut dev, REG_TEMPERATURE, 2);
        assert_eq!(word, 0x8000 | 53, "sign bit set, magnitude 53 (5.3C in 0.1C units)");
    }

    #[test]
    fn response_echoes_function_code_and_byte_count_first() {
        let mut dev = Am2320::new();
        dev.start(false);
        dev.write(0x03);
        dev.write(REG_HUMIDITY);
        dev.write(0x02);
        dev.start(true);
        assert_eq!(dev.read(true), 0x03);
        assert_eq!(dev.read(true), 0x04);
    }

    #[test]
    fn a_wake_probe_is_harmless_and_does_not_desync_the_following_command() {
        let mut dev = Am2320::new();
        // 1-byte master-receive (wake), then 0-byte master-transmit (wake).
        dev.start(true);
        let _dummy = dev.read(true);
        dev.stop();
        dev.start(false);
        dev.stop();

        assert_eq!(read_command(&mut dev, REG_TEMPERATURE, 2), 250);
    }
}
