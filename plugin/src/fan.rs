//! MAX31760 fan controller, modeled to CPLDIcy's actual on-board wiring
//! (docs/board-facts.md section 5): not memory-mapped through the CPLD
//! at all -- it's the same MAX31760 I2C chip the standalone "Fanny"
//! ISA card uses, addressed as another device on the shared bus, which
//! is what makes it register-compatible with the existing FannyCtl
//! driver/tool for free.
//!
//! Only the registers this board's teaching scenarios actually touch are
//! modeled: PWMR (ramp rate), PWMV (duty cycle), and Fan1's tach count
//! (TC1H/TC1L). CR1-3, the fault/threshold registers, the LUT, and the
//! EEPROM area (docs/board-facts.md §5's full register map) read back as
//! zero and accept writes with no effect -- nothing in docs/PLAN.md's
//! Phase 2 scope (the closed thermal loop demo) needs them, and adding
//! behavior nothing exercises would just be surface area to get wrong
//! silently. Fan2's tach registers mirror Fan1's: this emulation models
//! one virtual fan, not two independent ones.
//!
//! I2C address: 0x50 (7-bit) / 0xA0 (8-bit), the `FannyCtl` tool's own
//! documented default -- flagged low-confidence in
//! docs/board-facts.md §5/§8 pending oracle verification.

use crate::i2c::I2cDevice;

const REG_PWMR: u8 = 0x50;
const REG_PWMV: u8 = 0x51;
const REG_TC1H: u8 = 0x52;
const REG_TC1L: u8 = 0x53;
const REG_TC2H: u8 = 0x54;
const REG_TC2L: u8 = 0x55;

/// Standard I2C address for this board's MAX31760 -- see module docs for
/// the confidence caveat.
pub const MAX31760_ADDRESS: u8 = 0x50;

/// Emulated cck for the virtual fan to ramp from fully stopped to full
/// speed (or the reverse) -- arbitrary but nonzero, so a scenario's
/// temperature ramp -> fan-curve response demo (docs/PLAN.md section
/// 3.6) has an observable "spinning up" state in between, the same
/// reasoning as [`crate::pcf8584::CCK_PER_BYTE_PHASE`]: virtual timing
/// standing in for real physical inertia, not calibrated against a real
/// fan's actual spec.
const RAMP_CCK_TOTAL: u32 = 10_000;

/// A representative case fan's full-speed RPM, purely for producing a
/// plausible tach reading -- not tied to any real fan's datasheet.
const MAX_RPM: u32 = 3000;

/// MAX31760 tach formula (docs/board-facts.md §5): assumes 2 pulses/rev
/// and a 100kHz internal tach clock.
const TACH_CONSTANT: u32 = 3_000_000;

pub struct VirtualFan {
    duty: u8,
    current_rpm: u32,
    /// Fault knob (docs/PLAN.md section 3.4's "stuck rotor" fixture):
    /// the rotor never spins regardless of commanded duty.
    stuck: bool,
}

impl VirtualFan {
    pub fn new() -> Self {
        Self {
            duty: 0,
            current_rpm: 0,
            stuck: false,
        }
    }

    fn target_rpm(&self) -> u32 {
        if self.stuck {
            0
        } else {
            (u32::from(self.duty) * MAX_RPM) / 255
        }
    }

    pub fn set_duty(&mut self, duty: u8) {
        self.duty = duty;
    }

    pub fn duty(&self) -> u8 {
        self.duty
    }

    pub fn set_stuck(&mut self, stuck: bool) {
        self.stuck = stuck;
        if stuck {
            self.current_rpm = 0; // a seized rotor stops immediately, no ramp-down
        }
    }

    pub fn rpm(&self) -> u32 {
        self.current_rpm
    }

    /// Tach count a real MAX31760 would report for the current RPM:
    /// `0xFFFF` (no pulses seen) when stopped, matching a real
    /// tachometer's "no signal" reading rather than a division by zero.
    pub fn tach_count(&self) -> u16 {
        match TACH_CONSTANT.checked_div(self.current_rpm) {
            Some(count) => count.min(0xFFFF) as u16,
            None => 0xFFFF,
        }
    }

    pub fn tick(&mut self, cck: u32) {
        let target = self.target_rpm();
        let step = (MAX_RPM * cck) / RAMP_CCK_TOTAL;
        let step = step.max(1); // always make progress on a nonzero tick
        if self.current_rpm < target {
            self.current_rpm = (self.current_rpm + step).min(target);
        } else if self.current_rpm > target {
            self.current_rpm = self.current_rpm.saturating_sub(step).max(target);
        }
    }
}

impl Default for VirtualFan {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Max31760 {
    fan: VirtualFan,
    pwmr: u8,
    pointer: u8,
    awaiting_pointer: bool,
}

impl Max31760 {
    pub fn new() -> Self {
        Self {
            fan: VirtualFan::new(),
            pwmr: 0,
            pointer: 0,
            awaiting_pointer: false,
        }
    }

    pub fn fan(&self) -> &VirtualFan {
        &self.fan
    }

    pub fn fan_mut(&mut self) -> &mut VirtualFan {
        &mut self.fan
    }

    fn register_byte(&self, reg: u8) -> u8 {
        let tach = self.fan.tach_count();
        match reg {
            REG_PWMR => self.pwmr,
            REG_PWMV => self.fan.duty(),
            REG_TC1H | REG_TC2H => (tach >> 8) as u8,
            REG_TC1L | REG_TC2L => tach as u8,
            _ => 0x00,
        }
    }
}

impl Default for Max31760 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Max31760 {
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
        match self.pointer {
            REG_PWMR => self.pwmr = byte,
            REG_PWMV => self.fan.set_duty(byte),
            _ => {} // read-only/unmodeled register: ACK'd, no effect (module docs)
        }
        self.pointer = self.pointer.wrapping_add(1);
        true
    }

    fn read(&mut self, _master_will_ack: bool) -> u8 {
        let byte = self.register_byte(self.pointer);
        self.pointer = self.pointer.wrapping_add(1);
        byte
    }

    fn stop(&mut self) {}

    fn tick(&mut self, cck: u32) {
        self.fan.tick(cck);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_reg(dev: &mut Max31760, reg: u8, value: u8) {
        dev.start(false);
        dev.write(reg);
        dev.write(value);
    }

    fn read_reg(dev: &mut Max31760, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn pwmv_write_and_read_round_trip() {
        let mut dev = Max31760::new();
        write_reg(&mut dev, REG_PWMV, 0x80);
        assert_eq!(read_reg(&mut dev, REG_PWMV), 0x80);
    }

    #[test]
    fn a_stopped_fan_reports_zero_rpm_and_no_signal_tach() {
        let dev = Max31760::new();
        assert_eq!(dev.fan().rpm(), 0);
        assert_eq!(dev.fan().tach_count(), 0xFFFF);
    }

    #[test]
    fn spinning_up_is_gradual_not_instantaneous() {
        let mut dev = Max31760::new();
        write_reg(&mut dev, REG_PWMV, 0xFF); // full duty
        dev.tick(RAMP_CCK_TOTAL / 10);
        let partial_rpm = dev.fan().rpm();
        assert!(partial_rpm > 0, "should have started spinning up");
        assert!(partial_rpm < MAX_RPM, "shouldn't be at full speed yet");
    }

    #[test]
    fn full_duty_converges_to_max_rpm_and_the_tach_registers_reflect_it() {
        let mut dev = Max31760::new();
        write_reg(&mut dev, REG_PWMV, 0xFF);
        dev.tick(RAMP_CCK_TOTAL * 2); // well past full ramp

        assert_eq!(dev.fan().rpm(), MAX_RPM);

        let expected_tach = (TACH_CONSTANT / MAX_RPM) as u16;
        let tc1h = read_reg(&mut dev, REG_TC1H);
        let tc1l = read_reg(&mut dev, REG_TC1L);
        let tach = (u16::from(tc1h) << 8) | u16::from(tc1l);
        assert_eq!(tach, expected_tach);
    }

    #[test]
    fn stuck_rotor_fault_keeps_rpm_at_zero_regardless_of_duty() {
        let mut dev = Max31760::new();
        write_reg(&mut dev, REG_PWMV, 0xFF);
        dev.fan_mut().set_stuck(true);
        dev.tick(RAMP_CCK_TOTAL * 2);

        assert_eq!(dev.fan().rpm(), 0);
        assert_eq!(dev.fan().tach_count(), 0xFFFF);
    }

    #[test]
    fn fan_spins_down_gradually_when_duty_drops() {
        let mut dev = Max31760::new();
        write_reg(&mut dev, REG_PWMV, 0xFF);
        dev.tick(RAMP_CCK_TOTAL * 2);
        assert_eq!(dev.fan().rpm(), MAX_RPM);

        write_reg(&mut dev, REG_PWMV, 0x00);
        dev.tick(RAMP_CCK_TOTAL / 10);
        let spinning_down_rpm = dev.fan().rpm();
        assert!(spinning_down_rpm < MAX_RPM);
        assert!(spinning_down_rpm > 0);

        dev.tick(RAMP_CCK_TOTAL * 2);
        assert_eq!(dev.fan().rpm(), 0);
    }
}
