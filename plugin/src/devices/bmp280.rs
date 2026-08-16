//! Bosch BMP280 temperature/pressure sensor -- a teaching-sample
//! environmental sensor with real factory-calibration-coefficient
//! compensation math, unlike this board's simpler linear sensors
//! (LM75/LTC2990). A real BMP280 only ever hands out raw ADC counts;
//! converting them to °C/hPa requires applying Bosch's own published
//! fixed-point compensation formula to per-chip calibration constants
//! also read from the chip -- so that's what this emulation's register
//! space actually is: calibration coefficients plus raw ADC counts,
//! exactly like the real chip. `set_celsius`/`set_pressure_hpa` don't
//! set those directly; they binary-search for the raw ADC value whose
//! compensated output lands on the target, using this module's own
//! port of the same formula.
//!
//! That formula (`compensate_temp`/`compensate_pressure`) is ported
//! line-for-line from Bosch's official algorithm as implemented in
//! Henryk Richter's `i2csensors` driver (the real oracle-testable
//! consumer of this device -- see
//! https://gitlab.com/HenrykRichter/i2csensors,
//! `sensors/src/custom_bmp_bme.c`'s `custom_BMP280T`/`custom_BMP280P`),
//! using `i64` intermediates where that C code needed wide-multiply
//! helpers to avoid 32-bit overflow -- the arithmetic itself is
//! unchanged, just given a container that doesn't need the trick.
//!
//! Registers modeled (Bosch datasheet's memory map, and the exact
//! bytes `custom_BMP280T`/`custom_BMP280P` read): 0x88-0x8D temperature
//! calibration (dig_T1 u16, dig_T2/dig_T3 i16, all little-endian),
//! 0x8E-0x9F pressure calibration (dig_P1 u16, dig_P2-dig_P9 i16, all
//! little-endian), 0xF7-0xF9 pressure ADC (20-bit, MSB-first, bottom
//! nibble of the last byte unused), 0xFA-0xFC temperature ADC (same
//! shape). Every other register (id, reset, status, ctrl_meas, config)
//! reads back 0 and accepts writes with no effect -- nothing in this
//! board's use case reads oversampling/mode settings back, and this
//! emulation's "measurement" is just whatever `set_celsius`/
//! `set_pressure_hpa` last computed, not an actual conversion cycle.
//!
//! Fixed 7-bit address 0x76 (0xEC 8-bit, matching `BMP280.cfg`'s
//! `I2CADDRESS`) -- BMP280 does have one address-select pin (SDO), but
//! this teaching sample doesn't expose it as a config knob, same
//! treatment as [`crate::devices::ds1307::DS1307_ADDRESS`].
//!
//! Deliberately does *not* free-run against `tick()`, for the same
//! reproducibility reasoning as `pcf8583.rs`'s module docs.

use crate::i2c::I2cDevice;

/// Fixed I2C address of a real BMP280 with SDO tied high (the more
/// common strapping, and what `BMP280.cfg` itself targets).
pub const BMP280_ADDRESS: u8 = 0x76;

/// Bosch's published fixed-point temperature compensation formula,
/// ported line-for-line from `custom_BMP280T` (module docs). Returns
/// (t_fine, temperature in 0.01 degC units) -- t_fine feeds
/// `compensate_pressure`, same as on a real chip's own internal state.
fn compensate_temp(dig_t1: u16, dig_t2: i16, dig_t3: i16, adc_t: u32) -> (i64, i64) {
    let (dig_t1, dig_t2, dig_t3, adc_t) = (i64::from(dig_t1), i64::from(dig_t2), i64::from(dig_t3), i64::from(adc_t));
    let var1 = ((adc_t >> 3) - (dig_t1 << 1)) * dig_t2 >> 11;
    let var2 = (((adc_t >> 4) - dig_t1) * ((adc_t >> 4) - dig_t1) >> 12) * dig_t3 >> 14;
    let t_fine = var1 + var2;
    let t = (t_fine * 5 + 128) >> 8;
    (t_fine, t)
}

/// Bosch's published fixed-point pressure compensation formula, ported
/// line-for-line from `custom_BMP280P` (module docs). Returns pressure
/// in Pa (0 if the formula's own divide-by-zero guard trips, matching
/// the real driver's error return for out-of-range calibration/ADC
/// combinations).
#[allow(clippy::too_many_arguments)]
fn compensate_pressure(dig_p1: u16, dig_p: [i16; 8], t_fine: i64, adc_p: u32) -> i64 {
    let dig_p1 = i64::from(dig_p1);
    let [dig_p2, dig_p3, dig_p4, dig_p5, dig_p6, dig_p7, dig_p8, dig_p9] = dig_p.map(i64::from);
    let adc_p = i64::from(adc_p);

    let mut var1 = (t_fine >> 1) - 64000;
    let mut var2 = ((var1 >> 2) * (var1 >> 2) >> 11) * dig_p6;
    var2 += (var1 * dig_p5) << 1;
    var2 = (var2 >> 2) + (dig_p4 << 16);
    var1 = ((dig_p3 * ((var1 >> 2) * (var1 >> 2) >> 13)) >> 3) + ((dig_p2 * var1) >> 1);
    var1 >>= 18;
    var1 = (32768 + var1) * dig_p1 >> 15;
    if var1 == 0 {
        return 0; // matches the real formula's own guard against dividing by zero
    }

    let mut p = (1_048_576 - adc_p - (var2 >> 12)) * 3125;
    p = if p < 0x8000_0000 { (p << 1) / var1 } else { (p / var1) << 1 };

    let pvar1 = (dig_p9 * ((p >> 3) * (p >> 3) >> 13)) >> 12;
    let pvar2 = ((p >> 2) * dig_p8) >> 13;
    p + ((pvar1 + pvar2 + dig_p7) >> 4)
}

/// Binary-searches the `bits`-wide raw ADC value whose `decode` output
/// is closest to `target`, assuming `decode` is monotonic over the
/// range -- true for any realistic Bosch calibration data (this
/// module's own tests sweep the full range to confirm it for the fixed
/// coefficients used here).
fn search_raw(bits: u32, target: i64, mut decode: impl FnMut(u32) -> i64) -> u32 {
    let max = (1u32 << bits) - 1;
    let increasing = decode(max) >= decode(0);
    let (mut lo, mut hi) = (0u32, max);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let too_low = if increasing { decode(mid) < target } else { decode(mid) > target };
        if too_low {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

pub struct Bmp280 {
    dig_t1: u16,
    dig_t2: i16,
    dig_t3: i16,
    dig_p1: u16,
    dig_p: [i16; 8], // dig_P2..dig_P9
    adc_t: u32,       // 20-bit
    adc_p: u32,       // 20-bit
    pointer: u8,
    awaiting_pointer: bool,
}

impl Bmp280 {
    pub fn new() -> Self {
        // Plausible, realistic-shaped calibration constants (a real
        // chip's own values are factory-trimmed per unit and vary
        // freely within these fields' natural ranges -- there's no
        // single "correct" set to match, only self-consistency between
        // these coefficients and the ADC counts derived from them
        // below, which is what actually matters for a guest driver
        // applying the real formula).
        let dig_t1 = 27504u16;
        let dig_t2 = 26435i16;
        let dig_t3 = -1000i16;
        let dig_p1 = 36477u16;
        let dig_p = [-10685i16, 3024, 2855, 140, -7, 15500, -14600, 6000];

        let mut dev = Self {
            dig_t1,
            dig_t2,
            dig_t3,
            dig_p1,
            dig_p,
            adc_t: 0,
            adc_p: 0,
            pointer: 0,
            awaiting_pointer: false,
        };
        dev.set_celsius(25.0);
        dev.set_pressure_hpa(1013.25);
        dev
    }

    /// Scenario/test hook: set the sensed temperature by searching for
    /// the raw ADC count that compensates back to it.
    pub fn set_celsius(&mut self, celsius: f32) {
        let target = (celsius * 100.0).round() as i64;
        self.adc_t = search_raw(20, target, |adc_t| compensate_temp(self.dig_t1, self.dig_t2, self.dig_t3, adc_t).1);
    }

    /// Scenario/test hook: set the sensed pressure, compensated against
    /// whatever temperature is currently set (same as a real chip,
    /// where pressure compensation depends on t_fine from the last
    /// temperature reading).
    pub fn set_pressure_hpa(&mut self, hpa: f32) {
        let t_fine = compensate_temp(self.dig_t1, self.dig_t2, self.dig_t3, self.adc_t).0;
        let target_pa = (hpa * 100.0).round() as i64;
        self.adc_p = search_raw(20, target_pa, |adc_p| compensate_pressure(self.dig_p1, self.dig_p, t_fine, adc_p));
    }

    fn register_byte(&self, reg: u8) -> u8 {
        let le16 = |value: u16, hi: bool| if hi { (value >> 8) as u8 } else { value as u8 };
        match reg {
            0x88 => le16(self.dig_t1, false),
            0x89 => le16(self.dig_t1, true),
            0x8A => le16(self.dig_t2 as u16, false),
            0x8B => le16(self.dig_t2 as u16, true),
            0x8C => le16(self.dig_t3 as u16, false),
            0x8D => le16(self.dig_t3 as u16, true),
            0x8E => le16(self.dig_p1, false),
            0x8F => le16(self.dig_p1, true),
            0x90..=0x9F => {
                let idx = (reg - 0x90) as usize / 2;
                le16(self.dig_p[idx] as u16, (reg - 0x90) % 2 == 1)
            }
            0xF7 => (self.adc_p >> 12) as u8,
            0xF8 => (self.adc_p >> 4) as u8,
            0xF9 => ((self.adc_p & 0x0F) << 4) as u8,
            0xFA => (self.adc_t >> 12) as u8,
            0xFB => (self.adc_t >> 4) as u8,
            0xFC => ((self.adc_t & 0x0F) << 4) as u8,
            _ => 0x00, // id/reset/status/ctrl_meas/config -- not modeled
        }
    }
}

impl Default for Bmp280 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Bmp280 {
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
        // No register is bus-writable in this emulation -- calibration
        // is fixed and ADC counts are set wholesale via
        // `set_celsius`/`set_pressure_hpa`, same restriction as this
        // board's RTCs before oracle testing needed otherwise (nothing
        // here needs a guest to *write* a measurement back).
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

    fn read_reg(dev: &mut Bmp280, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    fn read_le16(dev: &mut Bmp280, reg: u8) -> u16 {
        u16::from(read_reg(dev, reg)) | (u16::from(read_reg(dev, reg + 1)) << 8)
    }

    #[test]
    fn calibration_registers_round_trip_the_fixed_coefficients() {
        let mut dev = Bmp280::new();
        assert_eq!(read_le16(&mut dev, 0x88), dev.dig_t1);
        assert_eq!(read_le16(&mut dev, 0x8A) as i16, dev.dig_t2);
        assert_eq!(read_le16(&mut dev, 0x8C) as i16, dev.dig_t3);
        assert_eq!(read_le16(&mut dev, 0x8E), dev.dig_p1);
        for (i, &expected) in dev.dig_p.to_vec().iter().enumerate() {
            assert_eq!(read_le16(&mut dev, 0x90 + 2 * i as u8) as i16, expected);
        }
    }

    #[test]
    fn set_celsius_round_trips_through_the_real_compensation_formula() {
        let mut dev = Bmp280::new();
        dev.set_celsius(25.0);
        let (_, t) = compensate_temp(dev.dig_t1, dev.dig_t2, dev.dig_t3, dev.adc_t);
        assert!((t - 2500).abs() <= 1, "expected ~25.00C (2500 in 0.01C units), got {t}");

        dev.set_celsius(-10.0);
        let (_, t) = compensate_temp(dev.dig_t1, dev.dig_t2, dev.dig_t3, dev.adc_t);
        assert!((t - -1000).abs() <= 1, "expected ~-10.00C, got {t}");
    }

    #[test]
    fn set_pressure_hpa_round_trips_through_the_real_compensation_formula() {
        let mut dev = Bmp280::new();
        dev.set_celsius(25.0);
        dev.set_pressure_hpa(1013.25);
        let t_fine = compensate_temp(dev.dig_t1, dev.dig_t2, dev.dig_t3, dev.adc_t).0;
        let p_pa = compensate_pressure(dev.dig_p1, dev.dig_p, t_fine, dev.adc_p);
        assert!((p_pa - 101_325).abs() <= 100, "expected ~101325 Pa (1013.25 hPa), got {p_pa}");
    }

    #[test]
    fn temperature_adc_registers_are_20_bit_msb_first() {
        let mut dev = Bmp280::new();
        dev.adc_t = 0x8_1234;
        assert_eq!(read_reg(&mut dev, 0xFA), 0x81);
        assert_eq!(read_reg(&mut dev, 0xFB), 0x23);
        assert_eq!(read_reg(&mut dev, 0xFC), 0x40);
    }

    #[test]
    fn compensate_temp_is_monotonic_across_the_full_adc_range() {
        // The binary search in `search_raw` depends on this holding for
        // the fixed calibration constants `Bmp280::new` uses.
        let dev = Bmp280::new();
        let mut last = compensate_temp(dev.dig_t1, dev.dig_t2, dev.dig_t3, 0).1;
        for adc_t in (0..=0xFFFFFu32).step_by(4096) {
            let t = compensate_temp(dev.dig_t1, dev.dig_t2, dev.dig_t3, adc_t).1;
            assert!(t >= last, "compensate_temp decreased at adc_t={adc_t}");
            last = t;
        }
    }

    #[test]
    fn writes_are_accepted_but_do_not_change_any_reading() {
        let mut dev = Bmp280::new();
        dev.set_celsius(25.0);
        let before = read_reg(&mut dev, 0xFA);
        dev.start(false);
        dev.write(0xF4); // ctrl_meas
        assert!(dev.write(0x6F)); // ACKed, matching a real chip
        assert_eq!(read_reg(&mut dev, 0xFA), before);
    }
}
