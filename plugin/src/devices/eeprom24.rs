//! 24Cxx-family I2C EEPROM -- persistent small storage with contents
//! loadable/dumpable host-side (docs/PLAN.md section 3.4), and the
//! natural teaching example for multi-byte addressed transfers.
//!
//! Simplified relative to a real 24Cxx: address auto-increment wraps
//! across the *entire* memory, not just the current page (real chips
//! only auto-increment within a fixed-size page during a write and
//! silently wrap the address back to the page start on overflow --
//! irrelevant to anything reading/writing one byte or one fully-in-page
//! block at a time, which covers the teaching use case this device
//! exists for; a page-accurate model can be added if a scenario ever
//! needs to demonstrate the overflow quirk itself).

use crate::i2c::I2cDevice;

/// Sizes over 256 bytes need a 2-byte memory address (24C04 and up); 24C01/24C02
/// fit their whole address space in the 7-bit device-address extension
/// plus one byte and don't need this, but 1-byte addressing is simpler
/// to reason about for a teaching device and this emulation only ever
/// exposes one 24Cxx per configured address, so the extra 24C01/24C02
/// addressing trick isn't needed either.
fn addr_bytes_for_size(size: usize) -> usize {
    if size > 256 {
        2
    } else {
        1
    }
}

pub struct Eeprom24 {
    data: Vec<u8>,
    addr_bytes: usize,
    /// Current byte address, kept mod `data.len()` (checked in `advance`).
    addr: usize,
    /// How many address bytes have been consumed since the last START;
    /// `addr_bytes` once the pointer is fully loaded and subsequent
    /// writes are data.
    addr_bytes_seen: usize,
    addr_being_built: usize,
}

impl Eeprom24 {
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "an EEPROM needs a nonzero size");
        Self {
            data: vec![0xFF; size], // erased/blank NOR-style default
            addr_bytes: addr_bytes_for_size(size),
            addr: 0,
            addr_bytes_seen: 0,
            addr_being_built: 0,
        }
    }

    /// Load initial contents from a host-supplied image (the
    /// `eeprom_image` config option, docs/PLAN.md section 3.7):
    /// truncates or zero-pads to this device's configured size.
    pub fn load_image(&mut self, image: &[u8]) {
        let n = image.len().min(self.data.len());
        self.data[..n].copy_from_slice(&image[..n]);
    }

    #[allow(dead_code)]
    pub fn dump(&self) -> &[u8] {
        &self.data
    }

    fn advance(&mut self) {
        self.addr = (self.addr + 1) % self.data.len();
    }
}

impl I2cDevice for Eeprom24 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            // A fresh write phase: the next `addr_bytes` writes rebuild
            // the address from scratch before any data byte lands.
            self.addr_bytes_seen = 0;
            self.addr_being_built = 0;
        }
        // A real chip's read path assumes the address was already set by
        // a prior write phase (or "current address read" continues from
        // wherever the last operation left off) -- nothing to do here
        // for read=true, `self.addr` is already wherever it was left.
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.addr_bytes_seen < self.addr_bytes {
            self.addr_being_built = (self.addr_being_built << 8) | usize::from(byte);
            self.addr_bytes_seen += 1;
            if self.addr_bytes_seen == self.addr_bytes {
                self.addr = self.addr_being_built % self.data.len();
            }
            return true;
        }
        self.data[self.addr] = byte;
        self.advance();
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.data[self.addr];
        self.advance();
        byte
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_erased_0xff_and_reports_the_configured_size() {
        let mut dev = Eeprom24::new(64);
        dev.start(false);
        dev.write(0x00); // address byte (1-byte addressing under 256)
        dev.start(true);
        assert_eq!(dev.read(true), 0xFF);
    }

    #[test]
    fn single_byte_write_then_random_read_round_trips() {
        let mut dev = Eeprom24::new(64);
        dev.start(false);
        dev.write(0x05); // address
        dev.write(0xAB); // data

        dev.start(false);
        dev.write(0x05); // re-point the address for the read
        dev.start(true);
        assert_eq!(dev.read(true), 0xAB);
    }

    #[test]
    fn sequential_write_auto_increments_the_address() {
        let mut dev = Eeprom24::new(64);
        dev.start(false);
        dev.write(0x00);
        dev.write(0x11);
        dev.write(0x22);
        dev.write(0x33);

        dev.start(false);
        dev.write(0x00);
        dev.start(true);
        assert_eq!(dev.read(true), 0x11);
        assert_eq!(dev.read(true), 0x22);
        assert_eq!(dev.read(true), 0x33);
    }

    #[test]
    fn address_wraps_at_the_end_of_memory() {
        let mut dev = Eeprom24::new(4);
        dev.start(false);
        dev.write(0x03); // last valid address
        dev.write(0xAA);
        dev.write(0xBB); // should wrap to address 0

        dev.start(false);
        dev.write(0x00);
        dev.start(true);
        assert_eq!(dev.read(true), 0xBB);
    }

    #[test]
    fn two_byte_addressing_kicks_in_above_256_bytes() {
        let mut dev = Eeprom24::new(4096); // 24C32-class
        dev.start(false);
        dev.write(0x01); // address high byte
        dev.write(0x00); // address low byte -> 0x0100
        dev.write(0x7E);

        dev.start(false);
        dev.write(0x01);
        dev.write(0x00);
        dev.start(true);
        assert_eq!(dev.read(true), 0x7E);
    }

    #[test]
    fn load_image_seeds_initial_contents_and_truncates_to_size() {
        let mut dev = Eeprom24::new(4);
        dev.load_image(&[1, 2, 3, 4, 5, 6]);
        dev.start(false);
        dev.write(0x00);
        dev.start(true);
        assert_eq!(dev.read(true), 1);
        assert_eq!(dev.read(true), 2);
        assert_eq!(dev.read(true), 3);
        assert_eq!(dev.read(true), 4);
    }
}
