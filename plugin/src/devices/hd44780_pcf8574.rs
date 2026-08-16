//! A "generic I2C LCD backpack": a PCF8574 GPIO expander wired to an
//! HD44780-compatible character LCD in 4-bit mode, the near-universal
//! way hobbyist 16x2/20x4 character displays get an I2C interface (no
//! vendor ships an HD44780 with I2C pins of its own -- this backpack
//! wiring is the de facto standard across essentially every such module
//! on the market, e.g.
//! <https://www.flywing-tech.com/blog/pcf8574-i2c-address-pinout-interfacing-guide-2026-update/>).
//! Modeled as one combined device (like [`crate::fan::Max31760`]) rather
//! than a generic [`crate::devices::pcf8574::Pcf8574`] plus a separate
//! observer, since nothing else needs to watch this particular PCF8574's
//! pins.
//!
//! PCF8574 pin -> LCD signal mapping (the de facto standard wiring,
//! cited above): P0=RS, P1=R/W (write-only in this emulation -- no
//! scenario needs to poll the busy flag, so R/W is accepted but not
//! acted on), P2=E (enable strobe), P3=backlight (1=on), P4-P7=DB4-DB7
//! (the LCD's 4-bit data bus). Each 8-bit instruction/data byte is sent
//! as two nibbles, high nibble first, each latched on E's falling edge
//! -- both the wiring and the two-nibble/high-first order are official
//! HD44780 facts (Hitachi's HD44780U datasheet, "Interfacing to the
//! MPU" / Figure 9, e.g. <https://cdn.sparkfun.com/assets/9/5/f/7/b/HD44780.pdf>).
//!
//! HD44780 instruction set modeled (same datasheet, Table 6): Clear
//! Display, Return Home, Entry Mode Set (I/D only -- S/display-shift
//! isn't modeled, matching this project's restraint on features nothing
//! exercises), Display On/Off Control (D only -- cursor/blink aren't
//! rendered anywhere so C/B are accepted but not tracked), Set CGRAM
//! Address (just switches data writes away from DDRAM -- custom
//! character *contents* aren't stored, since nothing renders glyphs),
//! and Set DDRAM Address. Cursor/Display Shift and Function Set are
//! accepted (ACKed, matching a real chip) but have no modeled effect.
//! The busy flag isn't modeled: this emulation completes every
//! instruction instantly, so nothing would ever observe it busy.
//!
//! DDRAM is a flat 80-byte array indexed directly by the address
//! counter (matching the chip's real capacity, datasheet's "Display
//! Data RAM" section); the standard 2-line split is address 0x00 for
//! row 0 and 0x40 for row 1 (Figure 4). Text is exposed via [`line`]
//! for whatever export path the host side wants (`host_log`-on-change
//! is what `board.rs`/`lib.rs` do with it) -- non-printable/non-ASCII
//! byte values (the chip's built-in Japanese/European character ROM
//! isn't modeled) render as `?`.

use crate::i2c::I2cDevice;

const RS: u8 = 0x01;
const EN: u8 = 0x04;
const BACKLIGHT: u8 = 0x08;

const DDRAM_LEN: usize = 80;
const ROW1_BASE: usize = 0x40;

/// Default I2C address of the PCF8574-based backpack (A0-A2 strapped
/// high) -- by far the most common out of the box; PCF8574A-based
/// backpacks instead default to 0x3F, adjustable via this device's own
/// `_address` config knob same as `eeprom`/`pcf8583`.
pub const HD44780_PCF8574_DEFAULT_ADDRESS: u8 = 0x27;

pub struct Hd44780Pcf8574 {
    ddram: [u8; DDRAM_LEN],
    cursor: u8,
    entry_increment: bool,
    display_on: bool,
    backlight: bool,
    cgram_mode: bool,
    output_latch: u8,
    /// The first nibble of a two-nibble transfer, once its EN falling
    /// edge has latched it in -- `None` while waiting for that first
    /// nibble.
    pending_high_nibble: Option<(bool, u8)>,
}

impl Hd44780Pcf8574 {
    pub fn new() -> Self {
        Self {
            ddram: [b' '; DDRAM_LEN],
            cursor: 0,
            entry_increment: true, // HD44780 reset default: I/D=1
            display_on: false,     // HD44780 reset default: display off
            backlight: true,       // PCF8574 reset default: all pins released high
            cgram_mode: false,
            output_latch: 0xFF, // PCF8574 reset default: all pins released high
            pending_high_nibble: None,
        }
    }

    pub fn is_display_on(&self) -> bool {
        self.display_on
    }

    pub fn is_backlight_on(&self) -> bool {
        self.backlight
    }

    /// Row 0 or row 1 (this emulation only models the standard 2-line
    /// DDRAM split), rendered as printable ASCII truncated/padded to
    /// `columns` characters (16 and 20 are the common physical widths;
    /// callers pass whatever their configured display size is).
    pub fn line(&self, row: usize, columns: usize) -> String {
        let base = if row == 0 { 0 } else { ROW1_BASE };
        let columns = columns.min(DDRAM_LEN - base);
        self.ddram[base..base + columns]
            .iter()
            .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '?' })
            .collect()
    }

    fn latch_nibble(&mut self, rs: bool, nibble: u8) {
        match self.pending_high_nibble.take() {
            None => self.pending_high_nibble = Some((rs, nibble)),
            Some((rs, high)) => self.execute(rs, (high << 4) | nibble),
        }
    }

    fn execute(&mut self, rs: bool, byte: u8) {
        if rs {
            self.write_data(byte);
        } else {
            self.execute_command(byte);
        }
    }

    fn write_data(&mut self, byte: u8) {
        if !self.cgram_mode {
            self.ddram[self.cursor as usize % DDRAM_LEN] = byte;
        }
        self.cursor = if self.entry_increment {
            self.cursor.wrapping_add(1)
        } else {
            self.cursor.wrapping_sub(1)
        };
    }

    fn execute_command(&mut self, cmd: u8) {
        // Checked narrowest-mask-first, matching the datasheet's
        // instruction table top-to-bottom -- the fixed-bit prefixes
        // never actually overlap, so the order doesn't change behavior,
        // just mirrors the table for readability.
        if cmd == 0x01 {
            self.ddram = [b' '; DDRAM_LEN];
            self.cursor = 0;
        } else if cmd & 0xFE == 0x02 {
            self.cursor = 0; // Return Home
        } else if cmd & 0xFC == 0x04 {
            self.entry_increment = cmd & 0x02 != 0; // Entry Mode Set
        } else if cmd & 0xF8 == 0x08 {
            self.display_on = cmd & 0x04 != 0; // Display On/Off Control
        } else if cmd & 0xF0 == 0x10 {
            // Cursor/Display Shift -- not modeled.
        } else if cmd & 0xE0 == 0x20 {
            // Function Set -- not modeled beyond acceptance.
        } else if cmd & 0xC0 == 0x40 {
            self.cgram_mode = true; // Set CGRAM Address
        } else {
            // Set DDRAM Address (cmd & 0x80 == 0x80, the only remaining case).
            self.cgram_mode = false;
            self.cursor = cmd & 0x7F;
        }
    }
}

impl Default for Hd44780Pcf8574 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Hd44780Pcf8574 {
    fn start(&mut self, _read: bool) -> bool {
        // A PCF8574 always ACKs its own address.
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        let was_en = self.output_latch & EN != 0;
        self.output_latch = byte;
        self.backlight = byte & BACKLIGHT != 0;
        if was_en && byte & EN == 0 {
            let rs = byte & RS != 0;
            let nibble = byte >> 4;
            self.latch_nibble(rs, nibble);
        }
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        self.output_latch
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives one nibble transfer the way a real backpack driver does:
    /// write with EN high, then the same byte with EN low.
    fn send_nibble(dev: &mut Hd44780Pcf8574, rs: bool, nibble: u8, backlight: u8) {
        let base = (if rs { RS } else { 0 }) | (nibble << 4) | backlight;
        dev.write(base | EN);
        dev.write(base);
    }

    fn send_byte(dev: &mut Hd44780Pcf8574, rs: bool, byte: u8) {
        send_nibble(dev, rs, byte >> 4, BACKLIGHT);
        send_nibble(dev, rs, byte & 0x0F, BACKLIGHT);
    }

    #[test]
    fn defaults_to_blank_display_off_and_backlight_on() {
        let dev = Hd44780Pcf8574::new();
        assert_eq!(dev.line(0, 16), " ".repeat(16));
        assert!(!dev.is_display_on());
        assert!(dev.is_backlight_on());
    }

    #[test]
    fn writing_a_data_byte_advances_the_cursor_and_appears_on_the_line() {
        let mut dev = Hd44780Pcf8574::new();
        for &b in b"Hi" {
            send_byte(&mut dev, true, b);
        }
        assert_eq!(dev.line(0, 16), "Hi              ");
    }

    #[test]
    fn set_ddram_address_selects_row_1() {
        let mut dev = Hd44780Pcf8574::new();
        send_byte(&mut dev, false, 0x80 | 0x40); // Set DDRAM address 0x40 -- row 1
        for &b in b"Row2" {
            send_byte(&mut dev, true, b);
        }
        assert_eq!(dev.line(1, 16), "Row2            ");
        assert_eq!(dev.line(0, 16), " ".repeat(16), "row 0 is untouched");
    }

    #[test]
    fn clear_display_blanks_ddram_and_resets_the_cursor() {
        let mut dev = Hd44780Pcf8574::new();
        send_byte(&mut dev, true, b'X');
        send_byte(&mut dev, false, 0x01); // Clear Display
        assert_eq!(dev.line(0, 16), " ".repeat(16));
        send_byte(&mut dev, true, b'Y');
        assert_eq!(dev.line(0, 16), "Y" .to_string() + &" ".repeat(15), "cursor is back at address 0 after clear");
    }

    #[test]
    fn display_on_off_control_is_tracked() {
        let mut dev = Hd44780Pcf8574::new();
        assert!(!dev.is_display_on());
        send_byte(&mut dev, false, 0x0C); // Display On/Off: D=1
        assert!(dev.is_display_on());
        send_byte(&mut dev, false, 0x08); // D=0
        assert!(!dev.is_display_on());
    }

    #[test]
    fn entry_mode_decrement_moves_the_cursor_backwards() {
        let mut dev = Hd44780Pcf8574::new();
        send_byte(&mut dev, false, 0x80 | 5); // Set DDRAM address 5
        send_byte(&mut dev, false, 0x04); // Entry Mode Set: I/D=0 (decrement)
        send_byte(&mut dev, true, b'Z');
        send_byte(&mut dev, true, b'A');
        let line = dev.line(0, 16);
        assert_eq!(&line[4..6], "AZ", "cursor moved 5 -> 4 -> 3 while writing");
    }

    #[test]
    fn set_cgram_address_diverts_data_writes_away_from_ddram() {
        let mut dev = Hd44780Pcf8574::new();
        send_byte(&mut dev, false, 0x80); // Set DDRAM address 0
        send_byte(&mut dev, false, 0x40); // Set CGRAM address 0 -- switches mode
        send_byte(&mut dev, true, 0xFF); // would corrupt DDRAM[0] if not diverted
        send_byte(&mut dev, false, 0x80); // Set DDRAM address 0 again
        assert_eq!(dev.line(0, 1), " ", "CGRAM writes never touched DDRAM");
    }

    #[test]
    fn backlight_bit_is_tracked_independently_of_lcd_instructions() {
        let mut dev = Hd44780Pcf8574::new();
        dev.write(BACKLIGHT); // no EN edge -- just the output latch
        assert!(dev.is_backlight_on());
        dev.write(0x00);
        assert!(!dev.is_backlight_on());
    }

    #[test]
    fn always_acks_its_address() {
        let mut dev = Hd44780Pcf8574::new();
        assert!(dev.start(false));
        assert!(dev.start(true));
    }
}
