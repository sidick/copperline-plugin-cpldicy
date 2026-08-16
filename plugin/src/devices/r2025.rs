//! Ricoh R2025 clock/calendar -- host-settable time, same teaching-
//! sample role as [`crate::devices::pcf8583::Pcf8583`], the fourth and
//! last `SetClockI2C`-supported RTC this board models (see the tool's
//! package docs at https://aminet.net/package/docs/hard/SetClockI2C).
//! Also bus-writable, same as the other three: Henryk Richter's
//! `i2clock` (part of the `i2csensors` repo,
//! https://gitlab.com/HenrykRichter/i2csensors, `i2cclass_rtc.c`'s
//! `i2c_RTCReadR2025`/`i2c_RTCWriteR2025`) targets this exact address
//! and writes the same 0x0-0x6 time-register block this device models
//! (alongside 0xE/0xF control-register and 0x7 oscillator-adjustment
//! writes this emulation doesn't act on -- see below).
//!
//! Registers modeled (R2025 datasheet's "Address Mapping" table,
//! addresses 0h-6h): 0x0 seconds, 0x1 minutes, 0x2 hours, 0x3
//! day-of-week, 0x4 date, 0x5 month, 0x6 year -- all BCD except
//! day-of-week (plain binary 0-6, correspondence to weekday names is
//! user-definable on a real chip, matching the datasheet's own
//! "Sunday = 0,0,0" example). Addresses 0x7 (oscillation adjustment)
//! and 0x8-0xF (alarm/control registers) aren't modeled -- writes to
//! them are accepted (ACKed) but discarded, same restraint as
//! `pcf8583.rs`'s general RAM area. In particular, the 12/24-hour mode
//! bit lives in the unmodeled Control Register 1 (address 0xE); this
//! emulation always presents the hour byte in 24-hour BCD regardless,
//! same convention as the other three RTCs here. The century bit
//! (address 0x5, D7) isn't tracked either, for the same "no scenario
//! needs it" reason `pcf8583.rs` doesn't track a full 4-digit year.
//!
//! Addressing is unlike the other three RTCs: the byte immediately
//! after the slave address packs a 4-bit address pointer in its high
//! nibble and a 4-bit "Transmission Format Register" in its low nibble
//! (datasheet §"Data Transmission Write Format" -- only format 0000,
//! "plain sequential access", is documented/supported, so this
//! emulation ignores the low nibble entirely rather than rejecting
//! other values). The pointer auto-increments (wrapping 0xF -> 0x0)
//! for every byte after that first one, on both writes and reads.
//!
//! Fixed 7-bit address 0x32 (`0110010`, the datasheet's own literal
//! slave address) -- the R2025 has no address-select pins, so unlike
//! `pcf8583`, this device carries no `_address` config knob, same
//! treatment as [`crate::devices::ds1307::DS1307_ADDRESS`].
//!
//! Deliberately does *not* free-run against `tick()`, for the same
//! reproducibility reasoning as `pcf8583.rs`'s module docs. Time only
//! advances when a scenario/test calls `set_time`, or the guest writes
//! it over the bus -- never on its own.

use crate::i2c::I2cDevice;

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn from_bcd(byte: u8) -> u8 {
    (byte >> 4) * 10 + (byte & 0x0F)
}

/// Fixed I2C address of every real R2025 -- no address pins to strap.
pub const R2025_ADDRESS: u8 = 0x32;

#[derive(Clone, Copy)]
pub struct DateTime {
    pub year: u8,    // 0-99 (no century byte tracked -- see module docs)
    pub month: u8,   // 1-12
    pub date: u8,    // 1-31
    pub weekday: u8, // 0-6, correspondence to weekday names is arbitrary
    pub hour: u8,    // 0-23 (24-hour mode only -- see module docs)
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub const EPOCH: Self = Self {
        year: 0,
        month: 1,
        date: 1,
        weekday: 0,
        hour: 0,
        minute: 0,
        second: 0,
    };
}

pub struct R2025 {
    time: DateTime,
    pointer: u8,
    awaiting_pointer: bool,
}

impl R2025 {
    pub fn new() -> Self {
        Self {
            time: DateTime::EPOCH,
            pointer: 0,
            awaiting_pointer: false,
        }
    }

    /// Scenario/test hook: set the clock to an arbitrary (possibly
    /// nonsensical) time, same role as `Pcf8583::set_time`.
    pub fn set_time(&mut self, time: DateTime) {
        self.time = time;
    }

    fn register_byte(&self, reg: u8) -> u8 {
        match reg {
            0x00 => to_bcd(self.time.second),
            0x01 => to_bcd(self.time.minute),
            0x02 => to_bcd(self.time.hour),
            0x03 => self.time.weekday,
            0x04 => to_bcd(self.time.date),
            0x05 => to_bcd(self.time.month),
            0x06 => to_bcd(self.time.year),
            _ => 0x00,
        }
    }

    fn write_register(&mut self, reg: u8, byte: u8) {
        match reg {
            0x00 => self.time.second = from_bcd(byte & 0x7F), // D7 is unused, mask defensively
            0x01 => self.time.minute = from_bcd(byte & 0x7F),
            0x02 => self.time.hour = from_bcd(byte & 0x3F), // mask off the unmodeled 12/24-mode bits
            0x03 => self.time.weekday = byte & 0x07,
            0x04 => self.time.date = from_bcd(byte & 0x3F),
            0x05 => self.time.month = from_bcd(byte & 0x1F), // mask off the unmodeled century bit
            0x06 => self.time.year = from_bcd(byte),
            _ => {} // oscillator-adjustment/alarm/control registers, not modeled
        }
    }
}

impl Default for R2025 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for R2025 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.awaiting_pointer = true;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.awaiting_pointer {
            // High nibble: address pointer. Low nibble: Transmission
            // Format Register -- only format 0000 is documented, so
            // it's not otherwise inspected (module docs).
            self.pointer = (byte >> 4) & 0x0F;
            self.awaiting_pointer = false;
            return true;
        }
        self.write_register(self.pointer, byte);
        self.pointer = (self.pointer + 1) & 0x0F;
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.register_byte(self.pointer);
        self.pointer = (self.pointer + 1) & 0x0F;
        byte
    }

    fn stop(&mut self) {
        // "The internal address pointer is set to Fh when the Stop
        // Condition is met" (datasheet §"Data transmission read format
        // of the R2025S/D") -- confirmed load-bearing by the oracle
        // pass: `i2clock`'s `i2c_RTCReadR2025` does a bare burst read
        // with no preceding pointer-set write at all, discarding its
        // first byte and expecting the second to be the seconds
        // register. That only lines up if a prior STOP (from the
        // preceding SAVE, or from power-on) already parked the pointer
        // at 0xF, so the read's first (discarded) byte is 0xF and the
        // next one is 0x0 (seconds). Without this reset, a real chip
        // (and this emulation, before this fix) would resume from
        // wherever the last transaction's pointer was left, reading
        // the wrong registers entirely.
        self.pointer = 0x0F;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_reg(dev: &mut R2025, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg << 4); // pointer nibble | format 0000
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn defaults_to_the_epoch() {
        let mut dev = R2025::new();
        assert_eq!(read_reg(&mut dev, 0x00), 0x00); // seconds
        assert_eq!(read_reg(&mut dev, 0x01), 0x00); // minutes
        assert_eq!(read_reg(&mut dev, 0x02), 0x00); // hours
        assert_eq!(read_reg(&mut dev, 0x03), 0x00); // weekday
    }

    #[test]
    fn set_time_round_trips_through_bcd_registers() {
        let mut dev = R2025::new();
        dev.set_time(DateTime {
            year: 99,
            month: 12,
            date: 31,
            weekday: 6,
            hour: 23,
            minute: 59,
            second: 58,
        });

        assert_eq!(read_reg(&mut dev, 0x00), 0x58); // seconds BCD
        assert_eq!(read_reg(&mut dev, 0x01), 0x59); // minutes BCD
        assert_eq!(read_reg(&mut dev, 0x02), 0x23); // hours BCD, 24h mode
        assert_eq!(read_reg(&mut dev, 0x03), 0x06); // weekday, plain binary
        assert_eq!(read_reg(&mut dev, 0x04), 0x31); // date BCD
        assert_eq!(read_reg(&mut dev, 0x05), 0x12); // month BCD
        assert_eq!(read_reg(&mut dev, 0x06), 0x99); // year BCD
    }

    #[test]
    fn a_stop_condition_parks_the_pointer_at_0xf_for_the_next_bare_burst_read() {
        // Reproduces `i2clock`'s actual real-world access pattern
        // (module docs on `stop`): set the pointer to something
        // mid-register, STOP, then read without ever writing a new
        // pointer -- the first byte back should be the (unmodeled)
        // register at 0xF, and the second should be seconds.
        let mut dev = R2025::new();
        dev.set_time(DateTime {
            second: 58,
            ..DateTime::EPOCH
        });
        dev.start(false);
        dev.write(0x30); // pointer -> 0x3 (weekday), well away from 0xF
        dev.stop();

        dev.start(true);
        assert_eq!(dev.read(true), 0x00, "register 0xF isn't modeled");
        assert_eq!(dev.read(true), 0x58, "second byte of the burst is seconds");
    }

    #[test]
    fn pointer_auto_increments_across_a_burst_read_and_wraps_at_the_nibble_boundary() {
        let mut dev = R2025::new();
        dev.set_time(DateTime {
            second: 58,
            ..DateTime::EPOCH
        });
        dev.start(false);
        dev.write(0x00); // pointer=0x0, format=0000
        dev.start(true);
        assert_eq!(dev.read(true), 0x58); // seconds
        assert_eq!(dev.read(true), 0x00); // minutes
        for _ in 0..14 {
            dev.read(true); // walk through hours..0xF
        }
        assert_eq!(dev.read(true), 0x58, "pointer wraps 0xF -> 0x0 back to seconds");
    }

    #[test]
    fn a_bus_write_to_the_time_registers_round_trips_through_a_read() {
        let mut dev = R2025::new();
        dev.start(false);
        dev.write(0x00); // pointer=0x0 (seconds), format=0000
        for byte in [0x58, 0x59, 0x23, 0x06, 0x31, 0x12, 0x99] {
            dev.write(byte); // seconds, minutes, hours, weekday, date, month, year
        }

        assert_eq!(read_reg(&mut dev, 0x00), 0x58);
        assert_eq!(read_reg(&mut dev, 0x01), 0x59);
        assert_eq!(read_reg(&mut dev, 0x02), 0x23);
        assert_eq!(read_reg(&mut dev, 0x03), 0x06);
        assert_eq!(read_reg(&mut dev, 0x04), 0x31);
        assert_eq!(read_reg(&mut dev, 0x05), 0x12);
        assert_eq!(read_reg(&mut dev, 0x06), 0x99);
    }

    #[test]
    fn writes_to_the_unmodeled_control_registers_are_accepted_but_discarded() {
        let mut dev = R2025::new();
        dev.start(false);
        dev.write(0xE0); // pointer=0xE (Control Register 1), format=0000
        assert!(dev.write(0x20)); // ACKed, matching a real chip
        assert_eq!(read_reg(&mut dev, 0x0E), 0x00, "not modeled -- always reads back 0");
    }

    #[test]
    fn time_never_advances_on_its_own() {
        let mut dev = R2025::new();
        dev.set_time(DateTime {
            second: 30,
            ..DateTime::EPOCH
        });
        dev.tick(1_000_000_000);
        assert_eq!(read_reg(&mut dev, 0x00), to_bcd(30));
    }
}
