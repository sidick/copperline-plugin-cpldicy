//! LM75-compatible temperature sensor -- the simplest teaching example
//! (docs/PLAN.md section 3.4), alongside the more elaborate LTC2990.
//!
//! Register set: 0x00 Temperature (read-only), 0x01 Configuration,
//! 0x02 T_hyst, 0x03 T_os. Only Temperature and Configuration are
//! modeled with any real behavior -- T_hyst/T_os exist so a driver
//! probing the register file doesn't find unmapped addresses, but this
//! emulation never triggers the OS/comparator output pin they configure
//! (no O.S. pin is wired anywhere in this board's virtual bus).
//!
//! Format (datasheet): 9-bit two's complement, 0.5C/LSB, left-justified
//! into a 16-bit big-endian pair (MSB = whole degrees, LSB's top bit =
//! the 0.5C fraction, remaining LSB bits always 0).

use crate::i2c::I2cDevice;

const REG_TEMP: u8 = 0x00;
const REG_CONFIG: u8 = 0x01;
const REG_THYST: u8 = 0x02;
const REG_TOS: u8 = 0x03;

pub struct Lm75 {
    celsius: f32,
    config: u8,
    thyst: i16, // raw 9-bit-in-16 format, same encoding as temperature
    tos: i16,
    pointer: u8,
    /// Byte position within whichever register is being streamed (0 or 1
    /// for the two-byte temp/thyst/tos registers; always 0 for the
    /// one-byte config register).
    byte_pos: u8,
    /// True immediately after START-for-write, before the pointer byte
    /// itself has been consumed.
    awaiting_pointer: bool,
}

impl Lm75 {
    pub fn new() -> Self {
        Self {
            celsius: 25.0,
            config: 0,
            thyst: encode_temp9(80.0),
            tos: encode_temp9(75.0),
            pointer: REG_TEMP,
            byte_pos: 0,
            awaiting_pointer: false,
        }
    }

    /// Scenario/test hook: set the sensed temperature.
    pub fn set_celsius(&mut self, celsius: f32) {
        self.celsius = celsius;
    }

    fn register_word(&self, reg: u8) -> i16 {
        match reg {
            REG_TEMP => encode_temp9(self.celsius),
            REG_THYST => self.thyst,
            REG_TOS => self.tos,
            _ => 0,
        }
    }

    fn read_byte_at(&self, reg: u8, pos: u8) -> u8 {
        if reg == REG_CONFIG {
            return self.config;
        }
        let word = self.register_word(reg) as u16;
        if pos == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }
}

impl Default for Lm75 {
    fn default() -> Self {
        Self::new()
    }
}

/// 9-bit two's complement, 0.5C/LSB, left-justified into a 16-bit word
/// (top 9 bits carry the value, bottom 7 bits always 0).
fn encode_temp9(celsius: f32) -> i16 {
    let raw = (celsius * 2.0).round() as i32;
    let clamped = raw.clamp(-256, 255); // 9-bit signed range
    (clamped as i16) << 7
}

impl I2cDevice for Lm75 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.awaiting_pointer = true;
        }
        self.byte_pos = 0;
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.awaiting_pointer {
            self.pointer = byte & 0x03;
            self.awaiting_pointer = false;
            return true;
        }
        // Data write: only Thyst/Tos are writable in practice; Config is
        // one byte, Thyst/Tos are two.
        match self.pointer {
            REG_CONFIG => self.config = byte,
            REG_THYST | REG_TOS => {
                let word = if self.pointer == REG_THYST {
                    self.thyst
                } else {
                    self.tos
                } as u16;
                let updated = if self.byte_pos == 0 {
                    (word & 0x00FF) | (u16::from(byte) << 8)
                } else {
                    (word & 0xFF00) | u16::from(byte)
                };
                if self.pointer == REG_THYST {
                    self.thyst = updated as i16;
                } else {
                    self.tos = updated as i16;
                }
                self.byte_pos = self.byte_pos.wrapping_add(1);
            }
            _ => {}
        }
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.read_byte_at(self.pointer, self.byte_pos);
        self.byte_pos = self.byte_pos.wrapping_add(1);
        byte
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_temp_word(dev: &mut Lm75) -> u16 {
        dev.start(false);
        dev.write(REG_TEMP);
        dev.start(true);
        let msb = dev.read(true);
        let lsb = dev.read(true);
        ((msb as u16) << 8) | lsb as u16
    }

    #[test]
    fn encodes_a_positive_half_degree_temperature() {
        let mut dev = Lm75::new();
        dev.set_celsius(25.5);
        // 25.5C -> raw 9-bit value 51 (25.5 / 0.5), left-justified: 51 << 7.
        assert_eq!(read_temp_word(&mut dev), (51i16 << 7) as u16);
    }

    #[test]
    fn encodes_a_negative_temperature_as_twos_complement() {
        let mut dev = Lm75::new();
        dev.set_celsius(-10.0);
        let word = read_temp_word(&mut dev) as i16;
        // -10.0C -> raw -20, left-justified: -20 << 7 = -2560.
        assert_eq!(word, -2560);
    }

    #[test]
    fn config_register_is_one_byte_and_round_trips() {
        let mut dev = Lm75::new();
        dev.start(false);
        dev.write(REG_CONFIG);
        dev.write(0x03);

        dev.start(false);
        dev.write(REG_CONFIG);
        dev.start(true);
        assert_eq!(dev.read(true), 0x03);
    }

    #[test]
    fn pointer_persists_across_repeated_start_for_current_register_reads() {
        let mut dev = Lm75::new();
        dev.set_celsius(30.0);
        dev.start(false);
        dev.write(REG_TEMP);
        // Repeated START straight into a read, no new pointer byte.
        dev.start(true);
        let msb = dev.read(true);
        // 30.0C -> raw 60, left-justified 60<<7 = 0x1E00; top byte 0x1E = 30.
        assert_eq!(msb, 30);
    }
}
