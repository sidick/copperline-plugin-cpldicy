//! PCF8574 8-bit quasi-bidirectional I/O expander -- the "blink an LED /
//! read a button" sample device (docs/PLAN.md section 3.4).
//!
//! Real hardware: each pin is an open-drain output with a weak internal
//! pull-up. Writing a bit to 0 drives that pin low; writing 1 releases it
//! (lets an external circuit pull it low or let it float high). Reading
//! always returns the actual pin state, which is the AND of what we're
//! driving and what's externally imposed -- so a pin written high can
//! still read low if something external (a button, this emulation's
//! `set_external_low`) is holding it down. This is what makes the chip
//! useful as both an output expander and an input expander on the same
//! 8 bits without any separate direction register.

use crate::i2c::I2cDevice;

pub struct Pcf8574 {
    /// What the master last wrote: 1 = released/high, 0 = driven low.
    output_latch: u8,
    /// Bits externally pulled low (buttons, jumpers, ...) -- not wired to
    /// the ABI yet (Phase 2's scenario/control surface will set this);
    /// present now so device unit tests can exercise the input side.
    external_low: u8,
}

impl Pcf8574 {
    pub fn new() -> Self {
        Self {
            output_latch: 0xFF,
            external_low: 0x00,
        }
    }

    fn pins(&self) -> u8 {
        self.output_latch & !self.external_low
    }

    /// Test/scenario hook: simulate an external device pulling bits low
    /// (bit set = held low), independent of what the master last wrote.
    #[allow(dead_code)]
    pub fn set_external_low(&mut self, mask: u8) {
        self.external_low = mask;
    }
}

impl Default for Pcf8574 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Pcf8574 {
    fn start(&mut self, _read: bool) -> bool {
        // A PCF8574 always ACKs its own address; there's no enable/disable
        // register to consult.
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        self.output_latch = byte;
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        self.pins()
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_all_pins_released_high() {
        let mut dev = Pcf8574::new();
        assert_eq!(dev.read(true), 0xFF);
    }

    #[test]
    fn write_sets_the_output_latch_which_read_reflects() {
        let mut dev = Pcf8574::new();
        assert!(dev.write(0b1010_0101));
        assert_eq!(dev.read(true), 0b1010_0101);
    }

    #[test]
    fn externally_held_low_bits_read_low_regardless_of_output_latch() {
        let mut dev = Pcf8574::new();
        dev.write(0xFF); // all released
        dev.set_external_low(0b0000_0001); // e.g. a button on bit 0
        assert_eq!(dev.read(true), 0b1111_1110);
    }

    #[test]
    fn a_driven_low_output_bit_wins_over_external_state() {
        let mut dev = Pcf8574::new();
        dev.write(0b1111_1110); // bit 0 driven low by the master
        dev.set_external_low(0b0000_0000); // nothing external pulling anything
        assert_eq!(dev.read(true), 0b1111_1110);
    }

    #[test]
    fn always_acks_its_address() {
        let mut dev = Pcf8574::new();
        assert!(dev.start(false));
        assert!(dev.start(true));
    }
}
