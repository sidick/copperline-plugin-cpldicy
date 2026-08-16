//! Bosch BME680 temperature/pressure/humidity sensor -- same
//! calibration-coefficients-plus-raw-ADC-counts shape as
//! [`crate::devices::bmp280::Bmp280`] (see that module's docs for why),
//! extended with a humidity channel. Gas resistance isn't modeled: the
//! real chip's gas compensation algorithm is Bosch proprietary and
//! isn't even implemented by this board's real oracle-testable
//! consumer either (Henryk Richter's `i2csensors` driver's own
//! `BME680.cfg` comment: "sadly, the gas computation method is kept
//! secret by Bosch"), so there's nothing to be compatible with there.
//!
//! All three formulas (`compensate_temp`/`compensate_pressure`/
//! `compensate_humidity`) are ported line-for-line from that same
//! driver (https://gitlab.com/HenrykRichter/i2csensors,
//! `sensors/src/custom_bmp_bme.c`'s `custom_BME680T`/`custom_BME680P`/
//! `custom_BME680H`), using `i64` intermediates in place of that C
//! code's wide-multiply helpers, same as `bmp280.rs`. Pressure and
//! humidity compensation both depend on `t_fine` from the temperature
//! reading (true on a real chip too), so `set_pressure_hpa`/
//! `set_humidity_percent` compute it from whatever `adc_t` is currently
//! set, same pattern as `bmp280.rs`'s `set_pressure_hpa`.
//!
//! Registers modeled (the exact bytes each `custom_BME680*` function
//! reads): 0x8A-0x8C temperature calibration (par_t2 i16 LE, par_t3
//! i8), 0xE9-0xEA more temperature calibration (par_t1 u16 LE -- yes,
//! split across two unrelated regions, a real quirk of this chip's
//! memory map, not a bug here), 0x8E-0xA0 pressure calibration
//! (par_p1-par_p10, mixed widths/signedness, see `register_byte`），
//! 0xE1-0xE8 humidity calibration (par_h1-par_h7, two of which are
//! nonstandard nibble-packed 12-bit fields -- see `register_byte`),
//! 0x22-0x24 temperature ADC (20-bit MSB-first), 0x1F-0x21 pressure ADC
//! (same shape), 0x25-0x26 humidity ADC (16-bit MSB-first, unsigned).
//! Every other register (id, reset, status, ctrl_* registers, gas
//! heater config) reads back 0 and accepts writes with no effect, same
//! restraint as `bmp280.rs`.
//!
//! Fixed 7-bit address 0x77 (0xEE 8-bit, matching `BME680.cfg`'s
//! `I2CADDRESS`) -- same no-config-address-knob treatment as
//! `bmp280.rs`'s BMP280_ADDRESS.
//!
//! Deliberately does *not* free-run against `tick()`, for the same
//! reproducibility reasoning as `pcf8583.rs`'s module docs.

use crate::i2c::I2cDevice;

/// Fixed I2C address of a real BME680 with SDO tied high (matching
/// `BME680.cfg`).
pub const BME680_ADDRESS: u8 = 0x77;

/// Ported from `custom_BME680T`. Returns (t_fine, temperature in 0.01
/// degC units) -- same final-step convention as `bmp280::compensate_temp`
/// (both are the standard Bosch family formula for this step).
fn compensate_temp(par_t1: u16, par_t2: i16, par_t3: i8, adc_t: u32) -> (i64, i64) {
    let (par_t1, par_t2, par_t3, adc_t) = (i64::from(par_t1), i64::from(par_t2), i64::from(par_t3), i64::from(adc_t));
    let var1 = (adc_t >> 3) - (par_t1 << 1);
    let var2 = (var1 * par_t2) >> 11;
    let var3 = (((var1 >> 1) * (var1 >> 1)) >> 12) * (par_t3 << 4) >> 14;
    let t_fine = var2 + var3;
    let t = (t_fine * 5 + 128) >> 8;
    (t_fine, t)
}

/// Ported from `custom_BME680P`. Returns pressure in Pa (0 if the
/// formula's own divide-by-zero guard trips).
#[allow(clippy::too_many_arguments)]
fn compensate_pressure(par_p1: u16, par_p: [i32; 9], t_fine: i64, adc_p: u32) -> i64 {
    let par_p1 = i64::from(par_p1);
    let [par_p2, par_p3, par_p4, par_p5, par_p6, par_p7, par_p8, par_p9, par_p10] = par_p.map(i64::from);
    let adc_p = i64::from(adc_p);

    let mut var1 = (t_fine >> 1) - 64000;
    let mut var2 = (((var1 >> 2) * (var1 >> 2)) >> 11) * par_p6 >> 2;
    var2 += (var1 * par_p5) << 1;
    var2 = (var2 >> 2) + (par_p4 << 16);
    var1 = ((((var1 >> 2) * (var1 >> 2)) >> 13) * (par_p3 << 5) >> 3) + ((par_p2 * var1) >> 1);
    var1 >>= 18;
    var1 = (32768 + var1) * par_p1 >> 15;
    if var1 == 0 {
        return 0;
    }

    let mut press_comp = (1_048_576 - adc_p - (var2 >> 12)) * 3125;
    press_comp = if press_comp < 0x4000_0000 { (press_comp << 1) / var1 } else { (press_comp / var1) << 1 };

    let pvar1 = (par_p9 * (((press_comp >> 3) * (press_comp >> 3)) >> 13) + 2048) >> 12;
    let pvar2 = ((press_comp >> 2) * par_p8 + 4096) >> 13;
    let pvar3 = (((press_comp >> 8) * par_p10) * ((press_comp >> 8) * (press_comp >> 8))) >> 17;
    press_comp + ((pvar1 + pvar2 + pvar3 + (par_p7 << 7)) >> 4)
}

/// Ported from `custom_BME680H`. Returns relative humidity as a percent
/// scaled by 65536 (matching the real formula's own internal
/// convention), clamped to `[0, 100*65536]`.
#[allow(clippy::too_many_arguments)]
fn compensate_humidity(par_h: [i32; 7], temp_centidegrees: i64, hum_adc: u32) -> i64 {
    let [par_h1, par_h2, par_h3, par_h4, par_h5, par_h6, par_h7] = par_h.map(i64::from);
    let hum_adc = i64::from(hum_adc);
    let temp_scaled = temp_centidegrees; // (t_fine*5+128)>>8, same value

    let var1 = (hum_adc - (par_h1 * 16)) - ((temp_scaled * par_h3 / 100) >> 1);
    let var2 = (par_h2 * ((temp_scaled * par_h4 / 100) + (((temp_scaled * (temp_scaled * par_h5 / 100)) >> 6) / 100) + (1 << 14))) >> 10;
    let var3 = var1 * var2;
    let mut var4 = par_h6 << 7;
    var4 = (var4 + (temp_scaled * par_h7 / 100)) >> 4;
    let var5 = ((var3 >> 14) * (var3 >> 14)) >> 10;
    let var6 = (var4 * var5) >> 1;

    (var3 + var6 + 32 >> 6).clamp(0, 100 * 65536)
}

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

pub struct Bme680 {
    par_t1: u16,
    par_t2: i16,
    par_t3: i8,
    par_p1: u16,
    par_p: [i32; 9], // par_p2..par_p10
    par_h: [i32; 7], // par_h1..par_h7
    adc_t: u32,       // 20-bit
    adc_p: u32,       // 20-bit
    hum_adc: u32,     // 16-bit
    pointer: u8,
    awaiting_pointer: bool,
}

impl Bme680 {
    pub fn new() -> Self {
        // Realistic-shaped calibration constants -- see bmp280.rs's
        // `new()` doc comment for why any self-consistent set works.
        let par_t1 = 26000u16;
        let par_t2 = 26000i16;
        let par_t3 = 3i8;
        let par_p1 = 34000u16;
        let par_p = [-10685i32, 88, 20, -7, 100, -20, 6000, 5, 30]; // par_p2..par_p10
        let par_h = [500i32, 200, 0, 30, 20, 120, 30]; // par_h1..par_h7 (par_h1/h2 are 12-bit unsigned)

        let mut dev = Self {
            par_t1,
            par_t2,
            par_t3,
            par_p1,
            par_p,
            par_h,
            adc_t: 0,
            adc_p: 0,
            hum_adc: 0,
            pointer: 0,
            awaiting_pointer: false,
        };
        dev.set_celsius(25.0);
        dev.set_pressure_hpa(1013.25);
        dev.set_humidity_percent(50.0);
        dev
    }

    fn temp_fine_and_centidegrees(&self) -> (i64, i64) {
        compensate_temp(self.par_t1, self.par_t2, self.par_t3, self.adc_t)
    }

    /// Scenario/test hook: set the sensed temperature by searching for
    /// the raw ADC count that compensates back to it.
    pub fn set_celsius(&mut self, celsius: f32) {
        let target = (celsius * 100.0).round() as i64;
        self.adc_t = search_raw(20, target, |adc_t| compensate_temp(self.par_t1, self.par_t2, self.par_t3, adc_t).1);
    }

    /// Scenario/test hook: set the sensed pressure, compensated against
    /// whatever temperature is currently set (same t_fine dependency a
    /// real chip has -- see module docs).
    pub fn set_pressure_hpa(&mut self, hpa: f32) {
        let (t_fine, _) = self.temp_fine_and_centidegrees();
        let target_pa = (hpa * 100.0).round() as i64;
        self.adc_p = search_raw(20, target_pa, |adc_p| compensate_pressure(self.par_p1, self.par_p, t_fine, adc_p));
    }

    /// Scenario/test hook: set the sensed relative humidity (0-100),
    /// compensated against whatever temperature is currently set.
    pub fn set_humidity_percent(&mut self, percent: f32) {
        let (_, t_centidegrees) = self.temp_fine_and_centidegrees();
        let target = (f64::from(percent) * 65536.0).round() as i64;
        self.hum_adc = search_raw(16, target, |hum_adc| compensate_humidity(self.par_h, t_centidegrees, hum_adc));
    }

    fn register_byte(&self, reg: u8) -> u8 {
        let le16 = |value: u16, hi: bool| if hi { (value >> 8) as u8 } else { value as u8 };
        match reg {
            0x8A => le16(self.par_t2 as u16, false),
            0x8B => le16(self.par_t2 as u16, true),
            0x8C => self.par_t3 as u8,
            0xE9 => le16(self.par_t1, false),
            0xEA => le16(self.par_t1, true),

            0x8E => le16(self.par_p1, false),
            0x8F => le16(self.par_p1, true),
            0x90 => le16(self.par_p[0] as u16, false), // par_p2
            0x91 => le16(self.par_p[0] as u16, true),
            0x92 => self.par_p[1] as u8, // par_p3
            0x94 => le16(self.par_p[2] as u16, false), // par_p4
            0x95 => le16(self.par_p[2] as u16, true),
            0x96 => le16(self.par_p[3] as u16, false), // par_p5
            0x97 => le16(self.par_p[3] as u16, true),
            0x98 => self.par_p[5] as u8, // par_p7
            0x99 => self.par_p[4] as u8, // par_p6
            0x9C => le16(self.par_p[6] as u16, false), // par_p8
            0x9D => le16(self.par_p[6] as u16, true),
            0x9E => le16(self.par_p[7] as u16, false), // par_p9
            0x9F => le16(self.par_p[7] as u16, true),
            0xA0 => self.par_p[8] as u8, // par_p10

            0xE1 => (self.par_h[1] >> 4) as u8, // par_h2 high byte
            0xE2 => (((self.par_h[1] & 0xF) << 4) | (self.par_h[0] & 0xF)) as u8,
            0xE3 => (self.par_h[0] >> 4) as u8, // par_h1 high byte
            0xE4 => self.par_h[2] as u8,        // par_h3
            0xE5 => self.par_h[3] as u8,        // par_h4
            0xE6 => self.par_h[4] as u8,        // par_h5
            0xE7 => self.par_h[5] as u8,        // par_h6
            0xE8 => self.par_h[6] as u8,        // par_h7

            0x1F => (self.adc_p >> 12) as u8,
            0x20 => (self.adc_p >> 4) as u8,
            0x21 => ((self.adc_p & 0x0F) << 4) as u8,
            0x22 => (self.adc_t >> 12) as u8,
            0x23 => (self.adc_t >> 4) as u8,
            0x24 => ((self.adc_t & 0x0F) << 4) as u8,
            0x25 => (self.hum_adc >> 8) as u8,
            0x26 => self.hum_adc as u8,

            _ => 0x00, // id/reset/status/ctrl_*/gas heater config -- not modeled
        }
    }
}

impl Default for Bme680 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Bme680 {
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
        // No register is bus-writable in this emulation, same
        // restriction as bmp280.rs.
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

    fn read_reg(dev: &mut Bme680, reg: u8) -> u8 {
        dev.start(false);
        dev.write(reg);
        dev.start(true);
        dev.read(true)
    }

    #[test]
    fn set_celsius_round_trips_through_the_real_compensation_formula() {
        let mut dev = Bme680::new();
        dev.set_celsius(25.0);
        let (_, t) = compensate_temp(dev.par_t1, dev.par_t2, dev.par_t3, dev.adc_t);
        assert!((t - 2500).abs() <= 1, "expected ~25.00C, got {t}");
    }

    #[test]
    fn set_pressure_hpa_round_trips_through_the_real_compensation_formula() {
        let mut dev = Bme680::new();
        dev.set_celsius(25.0);
        dev.set_pressure_hpa(1013.25);
        let (t_fine, _) = dev.temp_fine_and_centidegrees();
        let p_pa = compensate_pressure(dev.par_p1, dev.par_p, t_fine, dev.adc_p);
        assert!((p_pa - 101_325).abs() <= 100, "expected ~101325 Pa, got {p_pa}");
    }

    #[test]
    fn set_humidity_percent_round_trips_through_the_real_compensation_formula() {
        let mut dev = Bme680::new();
        dev.set_celsius(25.0);
        dev.set_humidity_percent(45.0);
        let (_, t_centi) = dev.temp_fine_and_centidegrees();
        let h = compensate_humidity(dev.par_h, t_centi, dev.hum_adc);
        let percent = h as f64 / 65536.0;
        assert!((percent - 45.0).abs() <= 0.1, "expected ~45.0%, got {percent}");
    }

    #[test]
    fn humidity_calibration_nibble_packing_round_trips() {
        let mut dev = Bme680::new();
        // par_h1/par_h2 are 12-bit unsigned, packed across regs
        // 0xE1-0xE3 -- confirm the real driver's exact unpacking
        // formula recovers what register_byte packed.
        let e1 = u32::from(read_reg(&mut dev, 0xE1));
        let e2 = u32::from(read_reg(&mut dev, 0xE2));
        let e3 = u32::from(read_reg(&mut dev, 0xE3));
        let par_h1 = (e3 << 4) + (e2 & 0xF);
        let par_h2 = (e1 << 4) + (e2 >> 4);
        assert_eq!(par_h1 as i32, dev.par_h[0]);
        assert_eq!(par_h2 as i32, dev.par_h[1]);
    }

    #[test]
    fn compensate_temp_is_monotonic_across_the_full_adc_range() {
        let dev = Bme680::new();
        let mut last = compensate_temp(dev.par_t1, dev.par_t2, dev.par_t3, 0).1;
        for adc_t in (0..=0xFFFFFu32).step_by(4096) {
            let t = compensate_temp(dev.par_t1, dev.par_t2, dev.par_t3, adc_t).1;
            assert!(t >= last, "compensate_temp decreased at adc_t={adc_t}");
            last = t;
        }
    }

    #[test]
    fn compensate_pressure_is_monotonic_across_the_full_adc_range() {
        // Confirms the fixed calibration constants `Bme680::new` uses
        // keep pressure a well-behaved (here: decreasing) function of
        // adc_p, same requirement `search_raw` has for temperature.
        let dev = Bme680::new();
        let (t_fine, _) = dev.temp_fine_and_centidegrees();
        let mut last = compensate_pressure(dev.par_p1, dev.par_p, t_fine, 0);
        let mut decreased_at_least_once = false;
        for adc_p in (0..=0xFFFFFu32).step_by(4096) {
            let p = compensate_pressure(dev.par_p1, dev.par_p, t_fine, adc_p);
            assert!(p <= last, "compensate_pressure increased at adc_p={adc_p}");
            decreased_at_least_once |= p < last;
            last = p;
        }
        assert!(decreased_at_least_once);
    }

    #[test]
    fn compensate_humidity_is_monotonic_across_the_full_adc_range() {
        let dev = Bme680::new();
        let (_, t_centi) = dev.temp_fine_and_centidegrees();
        let mut last = compensate_humidity(dev.par_h, t_centi, 0);
        for hum_adc in (0..=0xFFFFu32).step_by(256) {
            let h = compensate_humidity(dev.par_h, t_centi, hum_adc);
            assert!(h >= last, "compensate_humidity decreased at hum_adc={hum_adc}");
            last = h;
        }
    }

    #[test]
    fn temperature_adc_registers_are_20_bit_msb_first() {
        let mut dev = Bme680::new();
        dev.adc_t = 0x8_1234;
        assert_eq!(read_reg(&mut dev, 0x22), 0x81);
        assert_eq!(read_reg(&mut dev, 0x23), 0x23);
        assert_eq!(read_reg(&mut dev, 0x24), 0x40);
    }

    #[test]
    fn writes_are_accepted_but_do_not_change_any_reading() {
        let mut dev = Bme680::new();
        dev.set_celsius(25.0);
        let before = read_reg(&mut dev, 0x22);
        dev.start(false);
        dev.write(0x74); // ctrl_meas
        assert!(dev.write(0x25));
        assert_eq!(read_reg(&mut dev, 0x22), before);
    }
}
