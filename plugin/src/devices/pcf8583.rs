//! PCF8583 clock/calendar -- host-settable time, including deliberately
//! wrong time for testing a guest's correction logic (docs/PLAN.md
//! section 3.4), *and* bus-writable from the guest side -- unlike this
//! project's other RTCs, a real, unmodified guest tool needs to
//! round-trip a time write through this specific chip (see below).
//!
//! Registers modeled: 0x00 control/status, 0x01 hundredths-of-seconds,
//! 0x02 seconds, 0x03 minutes, 0x04 hours, 0x05 year/date, 0x06
//! weekday/month -- all BCD, matching the real chip. The general RAM
//! area (0x08-0xFF) and the event-counter/alarm modes aren't modeled,
//! with one exception: 0x10, which isn't an official PCF8583 register at
//! all, but is where Henryk Richter's `i2clock` (part of the
//! `i2csensors` repo, https://gitlab.com/HenrykRichter/i2csensors,
//! `i2cclass_rtc.c`'s `i2c_RTCReadPCF8583`/`i2c_RTCWritePCF8583`) stores
//! an extra rolling-year byte -- the chip's own year field is only 2
//! bits (a 4-year cycle), so that driver uses a spare RAM byte to track
//! which cycle it's in and reconstruct a real 4-digit year. Modeling
//! this one byte as plain read/write storage is what makes this device
//! oracle-testable against that real, unmodified tool rather than just
//! this crate's own tests.
//!
//! Deliberately does *not* free-run against `tick()`: a real PCF8583
//! ticks from its own 32.768kHz crystal, which has no meaningful
//! relationship to Copperline's emulated bus cycles, and free-running it
//! off `cck` would make every scenario involving this device
//! non-reproducible across different CPU/warp settings. Time only
//! advances when a scenario/test calls `set_time`, or the guest writes
//! it over the bus -- never on its own.

use crate::i2c::I2cDevice;

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn from_bcd(byte: u8) -> u8 {
    (byte >> 4) * 10 + (byte & 0x0F)
}

#[derive(Clone, Copy)]
pub struct DateTime {
    pub year_low2: u8, // PCF8583 only stores a 2-bit rolling year counter
    pub month: u8,     // 1-12
    pub date: u8,      // 1-31
    pub weekday: u8,   // 0-6
    pub hour: u8,      // 0-23 (24-hour mode only -- see module docs)
    pub minute: u8,
    pub second: u8,
    pub hundredths: u8,
}

impl DateTime {
    pub const EPOCH: Self = Self {
        year_low2: 0,
        month: 1,
        date: 1,
        weekday: 0,
        hour: 0,
        minute: 0,
        second: 0,
        hundredths: 0,
    };
}

pub struct Pcf8583 {
    control_status: u8,
    time: DateTime,
    /// Not an official PCF8583 register -- see module docs. Plain
    /// read/write storage; this emulation doesn't interpret its
    /// contents at all, just persists whatever a guest writes there.
    year_extra: u8,
    pointer: u8,
    awaiting_pointer: bool,
}

impl Pcf8583 {
    pub fn new() -> Self {
        Self {
            control_status: 0,
            time: DateTime::EPOCH,
            year_extra: 0,
            pointer: 0,
            awaiting_pointer: false,
        }
    }

    /// Scenario/test hook: set the clock to an arbitrary (possibly
    /// nonsensical) time, per docs/PLAN.md's "deliberately wrong time"
    /// fixture.
    pub fn set_time(&mut self, time: DateTime) {
        self.time = time;
    }

    fn register_byte(&self, reg: u8) -> u8 {
        match reg {
            0x00 => self.control_status,
            0x01 => to_bcd(self.time.hundredths),
            0x02 => to_bcd(self.time.second),
            0x03 => to_bcd(self.time.minute),
            // 24-hour mode only (bit 7 = 0): this emulation never sets
            // the 12/24 mode bit, so hour is plain BCD 0-23.
            0x04 => to_bcd(self.time.hour),
            // Year (top 2 bits, BCD-irrelevant rolling counter) | date
            // (bottom 6 bits, BCD 01-31).
            0x05 => (self.time.year_low2 << 6) | to_bcd(self.time.date),
            // Weekday (top 3 bits) | month (bottom 5 bits, BCD 01-12).
            0x06 => (self.time.weekday << 5) | to_bcd(self.time.month),
            0x10 => self.year_extra,
            _ => 0x00,
        }
    }

    fn write_register(&mut self, reg: u8, byte: u8) {
        match reg {
            0x00 => self.control_status = byte,
            0x01 => self.time.hundredths = from_bcd(byte),
            0x02 => self.time.second = from_bcd(byte),
            0x03 => self.time.minute = from_bcd(byte),
            0x04 => self.time.hour = from_bcd(byte & 0x3F), // mask off the unmodeled 12/24-mode bits
            0x05 => {
                self.time.year_low2 = byte >> 6;
                self.time.date = from_bcd(byte & 0x3F);
            }
            0x06 => {
                self.time.weekday = byte >> 5;
                self.time.month = from_bcd(byte & 0x1F);
            }
            0x10 => self.year_extra = byte,
            _ => {} // general RAM area, not modeled
        }
    }
}

impl Default for Pcf8583 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Pcf8583 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.awaiting_pointer = true;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.awaiting_pointer {
            self.pointer = byte;
            self.awaiting_pointer = false;
            return true;
        }
        self.write_register(self.pointer, byte);
        self.pointer = self.pointer.wrapping_add(1);
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.register_byte(self.pointer);
        self.pointer = self.pointer.wrapping_add(1);
        byte
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_reg(dev: &mut Pcf8583, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn defaults_to_the_epoch() {
        let mut dev = Pcf8583::new();
        assert_eq!(read_reg(&mut dev, 0x02), 0x00); // seconds
        assert_eq!(read_reg(&mut dev, 0x03), 0x00); // minutes
        assert_eq!(read_reg(&mut dev, 0x04), 0x00); // hours
    }

    #[test]
    fn set_time_round_trips_through_bcd_registers() {
        let mut dev = Pcf8583::new();
        dev.set_time(DateTime {
            year_low2: 2,
            month: 12,
            date: 31,
            weekday: 3,
            hour: 23,
            minute: 59,
            second: 58,
            hundredths: 12,
        });

        assert_eq!(read_reg(&mut dev, 0x01), 0x12); // hundredths BCD
        assert_eq!(read_reg(&mut dev, 0x02), 0x58); // seconds BCD
        assert_eq!(read_reg(&mut dev, 0x03), 0x59); // minutes BCD
        assert_eq!(read_reg(&mut dev, 0x04), 0x23); // hours BCD, 24h mode
        assert_eq!(read_reg(&mut dev, 0x05), (2 << 6) | 0x31); // year<<6 | date BCD
        assert_eq!(read_reg(&mut dev, 0x06), (3 << 5) | 0x12); // weekday<<5 | month BCD
    }

    #[test]
    fn a_deliberately_wrong_time_reads_back_exactly_as_set() {
        // The fault-injection use case (docs/PLAN.md): month 13, date 39
        // -- nonsense no calendar produces, but this emulation doesn't
        // validate, exactly so a guest's correction logic has something
        // to actually correct. (39, not e.g. 40: to_bcd(40) = 0x40, which
        // doesn't fit the register's 6-bit date field at all -- even a
        // "deliberately wrong" fixture has to fit the wire format it's
        // deliberately-wrong *within*, same as a real chip's date field
        // genuinely cannot represent 40 in any form.)
        let mut dev = Pcf8583::new();
        dev.set_time(DateTime {
            month: 13,
            date: 39,
            ..DateTime::EPOCH
        });
        assert_eq!(read_reg(&mut dev, 0x06) & 0x1F, to_bcd(13));
        assert_eq!(read_reg(&mut dev, 0x05) & 0x3F, to_bcd(39));
    }

    #[test]
    fn control_status_register_is_writable() {
        let mut dev = Pcf8583::new();
        dev.start(false);
        dev.write(0x00);
        dev.write(0x80); // e.g. STOP bit

        assert_eq!(read_reg(&mut dev, 0x00), 0x80);
    }

    #[test]
    fn a_bus_write_to_the_time_registers_round_trips_through_a_read() {
        let mut dev = Pcf8583::new();
        dev.start(false);
        dev.write(0x01); // pointer -> hundredths
        for byte in [0x12, 0x58, 0x59, 0x23, (2 << 6) | 0x31, (3 << 5) | 0x12] {
            dev.write(byte); // hundredths, seconds, minutes, hours, year<<6|date, weekday<<5|month
        }

        assert_eq!(read_reg(&mut dev, 0x01), 0x12);
        assert_eq!(read_reg(&mut dev, 0x02), 0x58);
        assert_eq!(read_reg(&mut dev, 0x03), 0x59);
        assert_eq!(read_reg(&mut dev, 0x04), 0x23);
        assert_eq!(read_reg(&mut dev, 0x05), (2 << 6) | 0x31);
        assert_eq!(read_reg(&mut dev, 0x06), (3 << 5) | 0x12);
    }

    #[test]
    fn the_year_extra_byte_is_plain_persisted_storage() {
        // Not an official register (module docs) -- just needs to
        // round-trip whatever a real guest driver stores there.
        let mut dev = Pcf8583::new();
        assert_eq!(read_reg(&mut dev, 0x10), 0x00);
        dev.start(false);
        dev.write(0x10);
        dev.write(0xA7);
        assert_eq!(read_reg(&mut dev, 0x10), 0xA7);
    }

    #[test]
    fn time_never_advances_on_its_own() {
        let mut dev = Pcf8583::new();
        dev.set_time(DateTime {
            second: 30,
            ..DateTime::EPOCH
        });
        dev.tick(1_000_000_000);
        assert_eq!(read_reg(&mut dev, 0x02), to_bcd(30));
    }
}
