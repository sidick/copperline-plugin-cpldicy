//! DS1629 clock/calendar (the "3-in-1 Silicon TimeChip"'s RTC half --
//! host-settable time, same teaching-sample role as
//! [`crate::devices::pcf8583::Pcf8583`]/[`crate::devices::ds1307::Ds1307`]
//! but a third `SetClockI2C`-supported RTC (see the tool's package docs
//! at https://aminet.net/package/docs/hard/SetClockI2C).
//!
//! Unlike the PCF8583/DS1307, the DS1629 doesn't expose a flat
//! register-pointer address space at all: every access starts with an
//! 8-bit *function command* (Maxim's datasheet, `COMMAND SET` section,
//! e.g. Access Clock = 0xC0, Access Config = 0xACh, Read Temperature =
//! 0xAAh, ...) that selects which internal register bank the rest of
//! the transaction talks to -- clock, thermostat, SRAM, and config are
//! otherwise-unrelated address spaces. This emulation only implements
//! the Access Clock (0xC0) command, matching this project's "clock/
//! calendar reads" scope: any other command byte is accepted (ACKed,
//! same as a real DS1629 would) but reads back all-zero and ignores
//! further writes, the same "unmodeled surface reads as zero" choice
//! `fan.rs` makes for MAX31760 registers this board's scenarios don't
//! exercise.
//!
//! Registers modeled (Access Clock's 7-byte clock register, DS1629
//! datasheet Figure 2): 0x00 seconds (bit 7 = CH, clock-halt), 0x01
//! minutes, 0x02 hours, 0x03 day-of-week, 0x04 date, 0x05 month, 0x06
//! year -- all BCD except day-of-week (plain binary 1-7), matching the
//! real chip. No on-chip control register exists for the clock itself
//! (12/24-hour mode lives in the hour byte, not a separate register);
//! this emulation never sets that mode bit, so hour is plain BCD 0-23,
//! same convention as `pcf8583.rs`/`ds1307.rs`.
//!
//! Fixed 7-bit address 0x4F: the DS1629's 3 device-select bits are
//! hard-wired high (datasheet's `SLAVE ADDRESS` section), so unlike
//! `pcf8583`, this device carries no `_address` config knob -- same
//! treatment as [`crate::devices::ds1307::DS1307_ADDRESS`].
//!
//! Deliberately does *not* free-run against `tick()`, for the same
//! reproducibility reasoning as `pcf8583.rs`'s module docs. Time only
//! changes when a scenario or test explicitly sets it.

use crate::i2c::I2cDevice;

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Fixed I2C address of every real DS1629 -- its 3 device-select bits
/// are hard-wired high.
pub const DS1629_ADDRESS: u8 = 0x4F;

/// The only function command this emulation implements.
const ACCESS_CLOCK: u8 = 0xC0;

#[derive(Clone, Copy)]
pub struct DateTime {
    pub year: u8,    // 0-99 (no century byte on a real DS1629)
    pub month: u8,   // 1-12
    pub date: u8,    // 1-31
    pub weekday: u8, // 1-7
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

/// Where a transaction is in the DS1629's command/address protocol
/// (module docs): a command byte selects the register bank, and only
/// after Access Clock does a following byte become a register address.
enum State {
    AwaitingCommand,
    AwaitingClockPointer,
    ClockRegisterAccess,
    Unimplemented,
}

pub struct Ds1629 {
    time: DateTime,
    pointer: u8,
    state: State,
}

impl Ds1629 {
    pub fn new() -> Self {
        Self {
            time: DateTime::EPOCH,
            pointer: 0,
            state: State::AwaitingCommand,
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
            // never "halted" in any way that would change readback.
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
}

impl Default for Ds1629 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ds1629 {
    fn start(&mut self, read: bool) -> bool {
        // A repeated START for a read continues from wherever the
        // write phase above left the pointer/state; only a write-phase
        // START re-enters "expect a command byte" (module docs).
        if !read {
            self.state = State::AwaitingCommand;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        match self.state {
            State::AwaitingCommand => {
                self.state = if byte == ACCESS_CLOCK {
                    State::AwaitingClockPointer
                } else {
                    State::Unimplemented
                };
            }
            State::AwaitingClockPointer => {
                self.pointer = byte % 7;
                self.state = State::ClockRegisterAccess;
            }
            State::ClockRegisterAccess => {
                // No clock register is bus-writable in this emulation
                // (time is set wholesale via `set_time`, same
                // restriction as `Pcf8583`/`Ds1307`) -- just advance
                // the pointer so a multi-byte write sequence still
                // completes without desyncing a following read.
                self.pointer = (self.pointer + 1) % 7;
            }
            State::Unimplemented => {}
        }
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        match self.state {
            State::ClockRegisterAccess => {
                let byte = self.register_byte(self.pointer);
                self.pointer = (self.pointer + 1) % 7;
                byte
            }
            _ => 0x00,
        }
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_reg(dev: &mut Ds1629, reg: u8) -> u8 {
        dev.start(false);
        dev.write(ACCESS_CLOCK);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn defaults_to_the_epoch() {
        let mut dev = Ds1629::new();
        assert_eq!(read_reg(&mut dev, 0x00), 0x00); // seconds
        assert_eq!(read_reg(&mut dev, 0x01), 0x00); // minutes
        assert_eq!(read_reg(&mut dev, 0x02), 0x00); // hours
        assert_eq!(read_reg(&mut dev, 0x03), 0x01); // weekday
    }

    #[test]
    fn set_time_round_trips_through_bcd_registers() {
        let mut dev = Ds1629::new();
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
    fn an_unimplemented_command_is_acked_but_reads_back_zero() {
        let mut dev = Ds1629::new();
        dev.start(false);
        assert!(dev.write(0xAA)); // Read Temperature -- not modeled
        dev.start(true);
        assert_eq!(dev.read(true), 0x00);
    }

    #[test]
    fn a_sequential_read_walks_the_seven_clock_registers_and_wraps() {
        let mut dev = Ds1629::new();
        dev.set_time(DateTime {
            second: 58,
            ..DateTime::EPOCH
        });
        dev.start(false);
        dev.write(ACCESS_CLOCK);
        dev.write(0x00); // start at seconds
        dev.start(true);
        assert_eq!(dev.read(true), 0x58); // seconds
        assert_eq!(dev.read(true), 0x00); // minutes
        for _ in 0..5 {
            dev.read(true); // hours, weekday, date, month, year
        }
        assert_eq!(dev.read(true), 0x58, "pointer wraps back to seconds after year");
    }

    #[test]
    fn time_never_advances_on_its_own() {
        let mut dev = Ds1629::new();
        dev.set_time(DateTime {
            second: 30,
            ..DateTime::EPOCH
        });
        dev.tick(1_000_000_000);
        assert_eq!(read_reg(&mut dev, 0x00), to_bcd(30));
    }
}
