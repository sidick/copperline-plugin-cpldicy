//! LTC2990 quad I2C voltage/current/temperature monitor -- CPLDIcy's
//! *authentic* resident (docs/board-facts.md section 6): this is the
//! chip the real card carries, wired at I2C address 0x4C in the same
//! channel configuration as the a1k.org ICYv2 board, which is what
//! `simplesensors`/`Sensei` expect out of the box.
//!
//! Channel assignment (docs/board-facts.md §6, matching CPLDIcy's own
//! `.cfg` for this board -- not a generic LTC2990 configuration):
//! - Tint (0x04/0x05): internal die temperature.
//! - V1 (0x06/0x07): the 5V rail, single-ended.
//! - V2 (0x08/0x09): the 12V rail, single-ended.
//! - V3/V4 (0x0A-0x0D): external NPN-diode remote temperature pair.
//! - VCC (0x0E/0x0F): supply voltage.
//!
//! Simplifications, both flagged in docs/board-facts.md §8 as open
//! items rather than confirmed facts: the CONTROL register (0x01) is
//! stored but doesn't gate which channels report data (this emulation
//! always reports all five, regardless of what mode bits a driver
//! configures -- a real chip only converts the channels its CONTROL
//! register selects); and V4 (0x0C/0x0D) is left at zero/not-ready since
//! it wasn't confirmed whether CPLDIcy's external-diode pair uses one
//! register or spans both. STATUS always reports every channel ready
//! (no conversion-in-progress modeling) since this device's values are
//! scenario-set, not actually converted.

use crate::i2c::I2cDevice;

const REG_STATUS: u8 = 0x00;
const REG_CONTROL: u8 = 0x01;
const REG_TRIGGER: u8 = 0x02;
const REG_TINT_MSB: u8 = 0x04;
const REG_V1_MSB: u8 = 0x06;
const REG_V2_MSB: u8 = 0x08;
const REG_V3_MSB: u8 = 0x0A;
const REG_VCC_MSB: u8 = 0x0E;

// Native chip LSB (305.18uV) -- what the datasheet's VCC register uses
// directly. V1/V2 use *effective*, divider-compensated LSBs instead: the
// raw register actually holds the chip's native-LSB reading of the
// divided pin voltage, but a driver's config (this board's
// LTC2990.cfg -- docs/board-facts.md §6) recovers the original rail
// voltage by multiplying the raw signed count by these larger,
// rail-level LSBs (0.61mV for the 10k/10k 5V divider, 1.22mV for the
// 30.1k/10k 12V divider) instead of the native 305.18uV. Modeling the
// *rail* voltage directly against these effective LSBs is equivalent to
// (and much simpler than) modeling the real divider math, and is what
// actually needs to round-trip against that driver config.
const VCC_LSB: f32 = 0.00030518;
const V1_LSB: f32 = 0.00061; // 5V rail via 10k/10k divider
const V2_LSB: f32 = 0.00122; // 12V rail via 30.1k/10k divider
const VCC_OFFSET: f32 = 2.5;

/// 13-bit two's complement, 0.0625C/LSB, DATA_VALID flagged in the MSB
/// byte's top bit (docs/board-facts.md §6's generic-chip facts).
fn encode_temp13(celsius: f32) -> [u8; 2] {
    let raw = (celsius * 16.0).round() as i32;
    let clamped = raw.clamp(-4096, 4095);
    let msb = 0x80 | (((clamped >> 8) as u8) & 0x1F);
    let lsb = (clamped & 0xFF) as u8;
    [msb, lsb]
}

/// 14-bit *unsigned* magnitude, DATA_VALID flagged in the MSB byte's top
/// bit -- single-ended channels (V1-V4 solo, and VCC) are documented as
/// unsigned 0-3.5V-range (docs/board-facts.md §6), unlike the
/// differential/temperature channels, which are signed. Getting this
/// wrong (treating it as signed two's complement) caps the encodable
/// range at half of what it should be -- exactly the bug that first
/// version of this function had, caught by a rail-voltage round-trip
/// test that exceeded the artificially small signed range.
fn encode_voltage14_unsigned(volts: f32, lsb: f32) -> [u8; 2] {
    let raw = (volts / lsb).round().clamp(0.0, 16383.0) as u16;
    let msb = 0x80 | (((raw >> 8) as u8) & 0x3F);
    let lsb_byte = (raw & 0xFF) as u8;
    [msb, lsb_byte]
}

pub struct Ltc2990 {
    control: u8,
    tint_celsius: f32,
    v1_volts: f32,
    v2_volts: f32,
    external_temp_celsius: f32,
    vcc_volts: f32,
    pointer: u8,
    awaiting_pointer: bool,
}

impl Ltc2990 {
    pub fn new() -> Self {
        Self {
            control: 0,
            tint_celsius: 25.0,
            v1_volts: 5.0,
            v2_volts: 12.0,
            external_temp_celsius: 25.0,
            vcc_volts: 5.0,
            pointer: REG_STATUS,
            awaiting_pointer: false,
        }
    }

    pub fn set_tint(&mut self, celsius: f32) {
        self.tint_celsius = celsius;
    }
    pub fn set_v1(&mut self, volts: f32) {
        self.v1_volts = volts;
    }
    pub fn set_v2(&mut self, volts: f32) {
        self.v2_volts = volts;
    }
    pub fn set_external_temp(&mut self, celsius: f32) {
        self.external_temp_celsius = celsius;
    }
    pub fn set_vcc(&mut self, volts: f32) {
        self.vcc_volts = volts;
    }

    fn status_byte(&self) -> u8 {
        // Busy=0, all five modeled channels ready: T_INT(bit1), V1(bit2),
        // V2(bit3), V3/ext-temp(bit4), VCC(bit6). Bit5 (V4) left clear --
        // see module docs.
        0b0101_1110
    }

    fn register_byte(&self, reg: u8) -> u8 {
        let tint = encode_temp13(self.tint_celsius);
        let v1 = encode_voltage14_unsigned(self.v1_volts, V1_LSB);
        let v2 = encode_voltage14_unsigned(self.v2_volts, V2_LSB);
        let ext_temp = encode_temp13(self.external_temp_celsius);
        let vcc = encode_voltage14_unsigned(self.vcc_volts - VCC_OFFSET, VCC_LSB);
        match reg {
            REG_STATUS => self.status_byte(),
            REG_CONTROL => self.control,
            REG_TRIGGER => self.status_byte(), // datasheet: reading TRIGGER returns STATUS
            REG_TINT_MSB => tint[0],
            0x05 => tint[1],
            REG_V1_MSB => v1[0],
            0x07 => v1[1],
            REG_V2_MSB => v2[0],
            0x09 => v2[1],
            REG_V3_MSB => ext_temp[0],
            0x0B => ext_temp[1],
            0x0C | 0x0D => 0x00, // V4: not modeled, see module docs
            REG_VCC_MSB => vcc[0],
            0x0F => vcc[1],
            _ => 0xFF,
        }
    }
}

impl Default for Ltc2990 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ltc2990 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.awaiting_pointer = true;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.awaiting_pointer {
            self.pointer = byte & 0x0F;
            self.awaiting_pointer = false;
            return true;
        }
        if self.pointer == REG_CONTROL {
            self.control = byte;
        }
        // TRIGGER and the measurement registers are effectively
        // read-only in this emulation (no conversion timing to trigger);
        // writes to them are accepted (ACK'd) but ignored, matching a
        // real chip's behavior of ACKing writes to read-only addresses
        // without them having any effect.
        self.pointer = (self.pointer + 1) & 0x0F;
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.register_byte(self.pointer);
        self.pointer = (self.pointer + 1) & 0x0F;
        byte
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_word(dev: &mut Ltc2990, reg: u8) -> (u8, u8) {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        (dev.read(true), dev.read(true))
    }

    #[test]
    fn tint_encodes_as_thirteen_bit_with_data_valid_flag() {
        let mut dev = Ltc2990::new();
        dev.set_tint(45.0);
        let (msb, lsb) = read_word(&mut dev, REG_TINT_MSB);
        assert_ne!(msb & 0x80, 0, "DATA_VALID should be set");
        let raw = (((msb & 0x1F) as i32) << 8) | lsb as i32;
        let raw = if raw & 0x1000 != 0 { raw - 0x2000 } else { raw };
        assert_eq!(raw, (45.0 * 16.0) as i32);
    }

    #[test]
    fn v1_and_v2_encode_the_configured_rail_voltages() {
        let mut dev = Ltc2990::new();
        dev.set_v1(5.05);
        dev.set_v2(12.3);

        let (msb, lsb) = read_word(&mut dev, REG_V1_MSB);
        let raw = (((msb & 0x3F) as u32) << 8) | lsb as u32; // unsigned: no sign extension
        let volts = raw as f32 * V1_LSB;
        assert!((volts - 5.05).abs() < 0.001);

        let (msb, lsb) = read_word(&mut dev, REG_V2_MSB);
        let raw = (((msb & 0x3F) as u32) << 8) | lsb as u32;
        let volts = raw as f32 * V2_LSB;
        assert!((volts - 12.3).abs() < 0.001);
    }

    #[test]
    fn vcc_applies_the_two_point_five_volt_offset() {
        let mut dev = Ltc2990::new();
        dev.set_vcc(5.0);
        let (msb, lsb) = read_word(&mut dev, REG_VCC_MSB);
        let raw = (((msb & 0x3F) as u32) << 8) | lsb as u32; // unsigned
        let volts = raw as f32 * VCC_LSB + VCC_OFFSET;
        assert!((volts - 5.0).abs() < 0.001);
    }

    #[test]
    fn pointer_auto_increments_across_a_burst_read() {
        let mut dev = Ltc2990::new();
        dev.set_tint(10.0);
        dev.set_v1(5.0);

        // A burst read starting at Tint should walk straight into V1's
        // registers without a new pointer write in between.
        dev.start(false);
        dev.write(REG_TINT_MSB);
        dev.start(true);
        let burst: Vec<u8> = (0..4).map(|_| dev.read(true)).collect();

        let mut reference = Ltc2990::new();
        reference.set_tint(10.0);
        reference.set_v1(5.0);
        let expected = [
            reference.register_byte(REG_TINT_MSB),
            reference.register_byte(0x05),
            reference.register_byte(REG_V1_MSB),
            reference.register_byte(0x07),
        ];
        assert_eq!(burst, expected);
    }

    #[test]
    fn trigger_register_read_returns_status() {
        let mut dev = Ltc2990::new();
        dev.start(false);
        dev.write(REG_TRIGGER);
        dev.start(true);
        assert_eq!(dev.read(true), dev.status_byte());
    }

    #[test]
    fn control_register_round_trips() {
        let mut dev = Ltc2990::new();
        dev.start(false);
        dev.write(REG_CONTROL);
        dev.write(0x18);

        dev.start(false);
        dev.write(REG_CONTROL);
        dev.start(true);
        assert_eq!(dev.read(true), 0x18);
    }
}
