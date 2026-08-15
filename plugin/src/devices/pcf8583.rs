//! PCF8583 clock/calendar -- host-settable time, including deliberately
//! wrong time for testing a guest's correction logic (docs/PLAN.md
//! section 3.4).
//!
//! Registers modeled: 0x00 control/status, 0x01 hundredths-of-seconds,
//! 0x02 seconds, 0x03 minutes, 0x04 hours, 0x05 year/date, 0x06
//! weekday/month -- all BCD, matching the real chip. The RAM area
//! (0x08-0xFF on a real PCF8583) and the event-counter/alarm modes
//! aren't modeled: nothing in this board's use case reads them, and
//! docs/PLAN.md scopes this device to "clock/calendar reads", not the
//! chip's full feature set.
//!
//! Deliberately does *not* free-run against `tick()`: a real PCF8583
//! ticks from its own 32.768kHz crystal, which has no meaningful
//! relationship to Copperline's emulated bus cycles, and free-running it
//! off `cck` would make every scenario involving this device
//! non-reproducible across different CPU/warp settings. Time only
//! changes when a scenario or test explicitly sets it -- exactly what
//! "deliberately wrong time for testing correction logic" needs anyway.

use crate::i2c::I2cDevice;

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
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
    pointer: u8,
    awaiting_pointer: bool,
}

impl Pcf8583 {
    pub fn new() -> Self {
        Self {
            control_status: 0,
            time: DateTime::EPOCH,
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
            _ => 0x00,
        }
    }

    fn write_register(&mut self, reg: u8, byte: u8) {
        // Only control/status is writable from this emulation's
        // perspective -- time is set wholesale via `set_time`, not built
        // up one BCD register write at a time (a real chip does support
        // this, but no scenario in docs/PLAN.md needs it, and decoding
        // partial writes back into `DateTime` correctly needs the same
        // BCD-to-binary logic `set_time` callers already have for free).
        if reg == 0x00 {
            self.control_status = byte;
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
