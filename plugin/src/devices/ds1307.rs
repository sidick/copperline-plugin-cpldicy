//! DS1307 clock/calendar -- host-settable time, same teaching-sample role
//! as [`crate::devices::pcf8583::Pcf8583`] but modeling Maxim/Dallas's
//! chip instead of Philips's (both are `SetClockI2C`-supported RTCs --
//! see the tool's package docs at https://aminet.net/package/docs/hard/SetClockI2C).
//! Also bus-writable, same as `pcf8583.rs`: Henryk Richter's `i2clock`
//! (part of the `i2csensors` repo,
//! https://gitlab.com/HenrykRichter/i2csensors, `i2cclass_rtc.c`'s
//! `i2c_RTCReadDS1307`/`i2c_RTCWriteDS1307`) writes a full 7-byte time
//! block to registers 0x00-0x06 at this exact address, matching this
//! device's register layout byte-for-byte.
//!
//! Registers modeled: 0x00 seconds (bit 7 = CH, clock-halt), 0x01
//! minutes, 0x02 hours, 0x03 day-of-week, 0x04 date, 0x05 month, 0x06
//! year, 0x07 control -- all BCD except day-of-week (plain binary 1-7 on
//! a real DS1307) and control, matching the real chip. The 56-byte NV
//! RAM area (0x08-0x3F) isn't modeled, same reasoning as
//! `pcf8583.rs`: nothing in this board's use case reads it.
//!
//! Fixed 7-bit address 0x68 -- the DS1307 has no address pins, unlike
//! the PCF8583's A0/A1 -- so unlike `pcf8583`, this device carries no
//! `_address` config knob, the same treatment as
//! [`crate::fan::MAX31760_ADDRESS`].
//!
//! Deliberately does *not* free-run against `tick()`, for the same
//! reason as `pcf8583.rs`'s module docs: reproducibility across
//! CPU/warp settings. Time only advances when a scenario/test calls
//! `set_time`, or the guest writes it over the bus -- never on its own.

use crate::i2c::I2cDevice;

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn from_bcd(byte: u8) -> u8 {
    (byte >> 4) * 10 + (byte & 0x0F)
}

/// Fixed I2C address of every real DS1307 -- no address pins to strap.
pub const DS1307_ADDRESS: u8 = 0x68;

#[derive(Clone, Copy)]
pub struct DateTime {
    pub year: u8,    // 0-99 (no century byte on a real DS1307)
    pub month: u8,   // 1-12
    pub date: u8,    // 1-31
    pub weekday: u8, // 1-7 (a real DS1307 stores this as plain binary, not BCD)
    pub hour: u8,    // 0-23 (24-hour mode only -- see module docs)
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub const EPOCH: Self = Self {
        year: 0,
        month: 1,
        date: 1,
        weekday: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
}

pub struct Ds1307 {
    control: u8,
    time: DateTime,
    pointer: u8,
    awaiting_pointer: bool,
}

impl Ds1307 {
    pub fn new() -> Self {
        Self {
            control: 0,
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
            // Bit 7 (CH) always reads 0: this emulation's clock is
            // never "halted" in any way that would change readback --
            // see module docs on not free-running against `tick()`.
            0x00 => to_bcd(self.time.second),
            0x01 => to_bcd(self.time.minute),
            // 24-hour mode only (bit 6 = 0): this emulation never sets
            // the 12/24 mode bit, so hour is plain BCD 0-23.
            0x02 => to_bcd(self.time.hour),
            0x03 => self.time.weekday,
            0x04 => to_bcd(self.time.date),
            0x05 => to_bcd(self.time.month),
            0x06 => to_bcd(self.time.year),
            0x07 => self.control,
            _ => 0x00,
        }
    }

    fn write_register(&mut self, reg: u8, byte: u8) {
        match reg {
            // Mask off CH (module docs: never modeled as actually
            // halting anything) so it can't corrupt the BCD decode.
            0x00 => self.time.second = from_bcd(byte & 0x7F),
            0x01 => self.time.minute = from_bcd(byte & 0x7F),
            0x02 => self.time.hour = from_bcd(byte & 0x3F), // mask off the unmodeled 12/24-mode bits
            0x03 => self.time.weekday = byte & 0x07,
            0x04 => self.time.date = from_bcd(byte & 0x3F),
            0x05 => self.time.month = from_bcd(byte & 0x1F),
            0x06 => self.time.year = from_bcd(byte),
            0x07 => self.control = byte,
            _ => {} // general NV RAM area, not modeled
        }
    }
}

impl Default for Ds1307 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ds1307 {
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

    fn read_reg(dev: &mut Ds1307, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn defaults_to_the_epoch() {
        let mut dev = Ds1307::new();
        assert_eq!(read_reg(&mut dev, 0x00), 0x00); // seconds
        assert_eq!(read_reg(&mut dev, 0x01), 0x00); // minutes
        assert_eq!(read_reg(&mut dev, 0x02), 0x00); // hours
        assert_eq!(read_reg(&mut dev, 0x03), 0x01); // weekday
    }

    #[test]
    fn set_time_round_trips_through_bcd_registers() {
        let mut dev = Ds1307::new();
        dev.set_time(DateTime {
            year: 99,
            month: 12,
            date: 31,
            weekday: 5,
            hour: 23,
            minute: 59,
            second: 58,
        });

        assert_eq!(read_reg(&mut dev, 0x00), 0x58); // seconds BCD
        assert_eq!(read_reg(&mut dev, 0x01), 0x59); // minutes BCD
        assert_eq!(read_reg(&mut dev, 0x02), 0x23); // hours BCD, 24h mode
        assert_eq!(read_reg(&mut dev, 0x03), 0x05); // weekday, plain binary
        assert_eq!(read_reg(&mut dev, 0x04), 0x31); // date BCD
        assert_eq!(read_reg(&mut dev, 0x05), 0x12); // month BCD
        assert_eq!(read_reg(&mut dev, 0x06), 0x99); // year BCD
    }

    #[test]
    fn control_register_is_writable() {
        let mut dev = Ds1307::new();
        dev.start(false);
        dev.write(0x07);
        dev.write(0x10); // e.g. SQWE bit

        assert_eq!(read_reg(&mut dev, 0x07), 0x10);
    }

    #[test]
    fn a_bus_write_to_the_time_registers_round_trips_through_a_read() {
        let mut dev = Ds1307::new();
        dev.start(false);
        dev.write(0x00); // pointer -> seconds
        for byte in [0x58, 0x59, 0x23, 0x05, 0x31, 0x12, 0x99] {
            dev.write(byte); // seconds, minutes, hours, weekday, date, month, year
        }

        assert_eq!(read_reg(&mut dev, 0x00), 0x58);
        assert_eq!(read_reg(&mut dev, 0x01), 0x59);
        assert_eq!(read_reg(&mut dev, 0x02), 0x23);
        assert_eq!(read_reg(&mut dev, 0x03), 0x05);
        assert_eq!(read_reg(&mut dev, 0x04), 0x31);
        assert_eq!(read_reg(&mut dev, 0x05), 0x12);
        assert_eq!(read_reg(&mut dev, 0x06), 0x99);
    }

    #[test]
    fn the_ch_bit_is_masked_off_the_seconds_write_rather_than_corrupting_the_decode() {
        let mut dev = Ds1307::new();
        dev.start(false);
        dev.write(0x00);
        dev.write(0x80 | 0x58); // CH=1 (stop-clock flag) alongside seconds=:58

        assert_eq!(read_reg(&mut dev, 0x00), 0x58, "CH always reads 0 (module docs) and never taints the BCD digits");
    }

    #[test]
    fn time_never_advances_on_its_own() {
        let mut dev = Ds1307::new();
        dev.set_time(DateTime {
            second: 30,
            ..DateTime::EPOCH
        });
        dev.tick(1_000_000_000);
        assert_eq!(read_reg(&mut dev, 0x00), to_bcd(30));
    }
}
