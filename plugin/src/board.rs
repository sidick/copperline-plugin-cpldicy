//! CPLDIcy's board window: Zorro byte-lane decode and register mirroring
//! (docs/board-facts.md section 2), wired to a [`Pcf8584`] controller and
//! its [`I2cBusEngine`]. This is the only module that knows CPLDIcy's own
//! address wiring; [`Pcf8584`] itself is chip-generic.
//!
//! Two hardware facts drive the decode, both read directly off the CPLD
//! VHDL/PCB sources (docs/board-facts.md §2):
//!
//! - `BA1 <= A(1)`: the CPLD passes Zorro address bit A1 straight through
//!   to the PCF8584's own register-select pin. Nothing else in the CPLD
//!   decodes sub-addresses within the board's page, so this register
//!   select repeats every 4 bytes across the *entire* 64K window (a
//!   reading of "only A1 is decoded", not confirmed against a real board
//!   -- see docs/board-facts.md §8 item 1's sibling caveats). Practically
//!   irrelevant to `i2c.library`, which only ever touches the two
//!   canonical offsets, but modeled faithfully rather than assumed away.
//! - The PCF8584's 8-bit data bus sits on D15-D8 only (Zorro `/UDS`); the
//!   CPLD's `nDS` input is wired only to `/UDS`, `/LDS` is unconnected.
//!   So a byte access at an *odd* address (`/LDS`) never reaches the
//!   chip at all -- it reads open bus and writes are dropped, regardless
//!   of which register bit1 would otherwise select.
//!
//! Phase 2 adds the rest of the virtual bus (docs/PLAN.md section 3.4):
//! PCF8574 and LTC2990 carry over from Phase 1/the real board; EEPROM,
//! LM75, PCF8583, and the MAX31760 fan controller are newly wired here,
//! each individually enable-able through [`BoardConfig`]. Scenario-
//! controllable devices are held both as an [`crate::i2c::SharedDevice`]
//! on the bus (for the real I2C protocol path) and as a typed
//! `Rc<RefCell<_>>` handle directly on `Board` (for
//! [`crate::scenario`]'s "set the virtual temperature" calls) --
//! see `crate::i2c`'s module docs for why one instance needs two access
//! paths instead of two independent copies of device state.

use crate::devices::am2320::{Am2320, AM2320_ADDRESS};
use crate::devices::bme680::{Bme680, BME680_ADDRESS};
use crate::devices::bmp280::{Bmp280, BMP280_ADDRESS};
use crate::devices::ds1307::{self, Ds1307, DS1307_ADDRESS};
use crate::devices::ds1629::{self, Ds1629, DS1629_ADDRESS};
use crate::devices::eeprom24::Eeprom24;
use crate::devices::hd44780_pcf8574::{Hd44780Pcf8574, HD44780_PCF8574_DEFAULT_ADDRESS};
use crate::devices::lm75::Lm75;
use crate::devices::ltc2990::Ltc2990;
use crate::devices::pcf8583::{DateTime, Pcf8583};
use crate::devices::pcf8574::Pcf8574;
use crate::devices::r2025::{self, R2025, R2025_ADDRESS};
use crate::fan::{Max31760, MAX31760_ADDRESS};
use crate::i2c::{I2cBusEngine, SharedDevice};
use crate::pcf8584::Pcf8584;
use crate::rtc_time::WallClock;
use std::cell::RefCell;
use std::rc::Rc;

/// A read from an address the board doesn't drive: open bus.
const OPEN_BUS_BYTE: u8 = 0xFF;

/// Per-device enable/address configuration (docs/PLAN.md section 3.7's
/// manifest `[config]` schema). PCF8574/LTC2990/the fan controller
/// default on -- the GPIO expander because Phase 1's `i2c.library` gate
/// depends on it being there out of the box, LTC2990/fan because they're
/// the real board's own authentic residents (docs/board-facts.md §5-6).
/// EEPROM/LM75/PCF8583/DS1307/DS1629/R2025/the LCD/BMP280/BME680/AM2320
/// are opt-in teaching devices, off by default to keep an out-of-the-box
/// bus scan uncluttered.
pub struct BoardConfig {
    pub pcf8574_enabled: bool,
    pub pcf8574_address: u8,
    pub eeprom_enabled: bool,
    pub eeprom_address: u8,
    pub eeprom_size: usize,
    pub eeprom_image: Option<Vec<u8>>,
    pub lm75_enabled: bool,
    pub lm75_address: u8,
    pub ltc2990_enabled: bool,
    pub ltc2990_address: u8,
    pub pcf8583_enabled: bool,
    pub pcf8583_address: u8,
    pub pcf8583_time: Option<WallClock>,
    pub ds1307_enabled: bool,
    pub ds1307_time: Option<WallClock>,
    pub ds1629_enabled: bool,
    pub ds1629_time: Option<WallClock>,
    pub r2025_enabled: bool,
    pub r2025_time: Option<WallClock>,
    pub lcd_enabled: bool,
    pub lcd_address: u8,
    pub lcd_columns: usize,
    pub bmp280_enabled: bool,
    pub bme680_enabled: bool,
    pub am2320_enabled: bool,
    pub fan_enabled: bool,
    pub fan_address: u8,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            pcf8574_enabled: true,
            pcf8574_address: 0x20,
            eeprom_enabled: false,
            eeprom_address: 0x54,
            eeprom_size: 4096, // 24C32-class
            eeprom_image: None,
            lm75_enabled: false,
            lm75_address: 0x48,
            ltc2990_enabled: true,
            // 0x4C (0x98 8-bit): the ICYv2/CPLDIcy/Matze A3000DB wiring,
            // matching this board. Henryk Richter's `i2csensors` repo
            // (https://gitlab.com/HenrykRichter/i2csensors) documents two
            // other real boards wiring the same chip at different
            // addresses -- 0x9A (Fanny Card) and 0x9E (BFG9060/ClockIIC)
            // -- reachable via the `ltc2990_address` config option if you
            // want to simulate one of those instead.
            ltc2990_address: 0x4C, // docs/board-facts.md §6
            pcf8583_enabled: false,
            // 0x50 (0xA0/0xA1 8-bit), not 0x51: this is the only address
            // Henryk Richter's `i2clock` (part of the `i2csensors` repo,
            // https://gitlab.com/HenrykRichter/i2csensors) recognizes a
            // PCF8583 at (i2cclass_rtc.c's I2C_PHILIPSA0) -- matching it
            // is what makes this device oracle-testable against real,
            // unmodified guest software rather than just this crate's
            // own tests. Note this is the same address `fan_address`
            // defaults to (see that field): a real board can't populate
            // both a MAX31760 and a PCF8583-at-A0 at once either, so
            // enabling both here needs one of them moved via its
            // `_address` config option first.
            pcf8583_address: 0x50,
            pcf8583_time: None,
            ds1307_enabled: false,
            ds1307_time: None,
            ds1629_enabled: false,
            ds1629_time: None,
            r2025_enabled: false,
            r2025_time: None,
            lcd_enabled: false,
            lcd_address: HD44780_PCF8574_DEFAULT_ADDRESS,
            lcd_columns: 16, // 16x2 is the common physical size
            bmp280_enabled: false,
            bme680_enabled: false,
            am2320_enabled: false,
            fan_enabled: true,
            // 0x50 (0xA0 8-bit): the Fanny/CPLDIcy wiring, matching this
            // board. `i2csensors` also documents 0xA2 (an alternate Fanny
            // strapping) -- reachable via `fan_address` if you want to
            // simulate that instead, or to resolve a collision with
            // `pcf8583_address` (see that field's own doc comment).
            fan_address: MAX31760_ADDRESS,
        }
    }
}

pub struct Board {
    pcf: Pcf8584,
    bus: I2cBusEngine,
    // Held to keep the Rc alive and reachable for future scenario hooks
    // (a PCF8574 "button press" fault, an EEPROM dump-for-inspection
    // call) -- not read directly yet, unlike the other typed handles
    // below, which already have scenario-facing setters.
    #[allow(dead_code)]
    pcf8574: Option<Rc<RefCell<Pcf8574>>>,
    #[allow(dead_code)]
    eeprom: Option<Rc<RefCell<Eeprom24>>>,
    lm75: Option<Rc<RefCell<Lm75>>>,
    ltc2990: Option<Rc<RefCell<Ltc2990>>>,
    pcf8583: Option<Rc<RefCell<Pcf8583>>>,
    ds1307: Option<Rc<RefCell<Ds1307>>>,
    ds1629: Option<Rc<RefCell<Ds1629>>>,
    r2025: Option<Rc<RefCell<R2025>>>,
    lcd: Option<Rc<RefCell<Hd44780Pcf8574>>>,
    bmp280: Option<Rc<RefCell<Bmp280>>>,
    bme680: Option<Rc<RefCell<Bme680>>>,
    am2320: Option<Rc<RefCell<Am2320>>>,
    fan: Option<Rc<RefCell<Max31760>>>,
    /// Name -> configured address, for enabled devices only -- lets
    /// [`crate::scenario`] target a fault fixture ("unplug the sensor")
    /// by the same device name a scenario script uses, without needing
    /// to know raw I2C addresses.
    device_addresses: Vec<(&'static str, u8)>,
    lcd_columns: usize,
    /// What [`Board::lcd_text_if_changed`] last returned, so it can
    /// report only when the LCD's visible content actually changes --
    /// see that method's own docs for why this lives here instead of
    /// lib.rs, which is the only module allowed to call `host_log`.
    lcd_last_lines: Option<[String; 2]>,
}

/// Attaches a freshly built `Rc<RefCell<T>>` to the bus and returns the
/// same handle, so callers can do `self.field = attach(&mut bus, addr,
/// device)` and end up with one shared instance reachable both ways.
fn attach<T: crate::i2c::I2cDevice + 'static>(
    bus: &mut I2cBusEngine,
    addr: u8,
    device: T,
) -> Rc<RefCell<T>> {
    let shared = Rc::new(RefCell::new(device));
    bus.attach(addr, Box::new(SharedDevice(Rc::clone(&shared))));
    shared
}

/// Converts a parsed `<device>_time` config value into each RTC's own
/// `DateTime` shape -- each device truncates the year and remaps the
/// weekday to its own convention differently, per its module docs.
fn pcf8583_datetime(w: &WallClock) -> DateTime {
    DateTime {
        year_low2: (w.year % 4) as u8,
        month: w.month,
        date: w.date,
        weekday: w.weekday_sun0(),
        hour: w.hour,
        minute: w.minute,
        second: w.second,
        hundredths: 0,
    }
}

fn ds1307_datetime(w: &WallClock) -> ds1307::DateTime {
    ds1307::DateTime {
        year: (w.year % 100) as u8,
        month: w.month,
        date: w.date,
        weekday: w.weekday_sun0() + 1, // DS1307 weekday is 1-7, arbitrary correspondence
        hour: w.hour,
        minute: w.minute,
        second: w.second,
    }
}

fn ds1629_datetime(w: &WallClock) -> ds1629::DateTime {
    ds1629::DateTime {
        year: (w.year % 100) as u8,
        month: w.month,
        date: w.date,
        weekday: w.weekday_sun0() + 1, // DS1629 weekday is 1-7, arbitrary correspondence
        hour: w.hour,
        minute: w.minute,
        second: w.second,
    }
}

fn r2025_datetime(w: &WallClock) -> r2025::DateTime {
    r2025::DateTime {
        year: (w.year % 100) as u8,
        month: w.month,
        date: w.date,
        weekday: w.weekday_sun0(), // R2025 weekday is 0-6, arbitrary correspondence
        hour: w.hour,
        minute: w.minute,
        second: w.second,
    }
}

impl Board {
    pub fn new() -> Self {
        Self::with_config(BoardConfig::default())
    }

    pub fn with_config(config: BoardConfig) -> Self {
        let mut bus = I2cBusEngine::new();

        let pcf8574 = config
            .pcf8574_enabled
            .then(|| attach(&mut bus, config.pcf8574_address, Pcf8574::new()));

        let eeprom = config.eeprom_enabled.then(|| {
            let mut dev = Eeprom24::new(config.eeprom_size);
            if let Some(image) = &config.eeprom_image {
                dev.load_image(image);
            }
            attach(&mut bus, config.eeprom_address, dev)
        });

        let lm75 = config
            .lm75_enabled
            .then(|| attach(&mut bus, config.lm75_address, Lm75::new()));

        let ltc2990 = config
            .ltc2990_enabled
            .then(|| attach(&mut bus, config.ltc2990_address, Ltc2990::new()));

        let pcf8583 = config.pcf8583_enabled.then(|| {
            let mut dev = Pcf8583::new();
            if let Some(w) = &config.pcf8583_time {
                dev.set_time(pcf8583_datetime(w));
            }
            attach(&mut bus, config.pcf8583_address, dev)
        });

        let ds1307 = config.ds1307_enabled.then(|| {
            let mut dev = Ds1307::new();
            if let Some(w) = &config.ds1307_time {
                dev.set_time(ds1307_datetime(w));
            }
            attach(&mut bus, DS1307_ADDRESS, dev)
        });

        let ds1629 = config.ds1629_enabled.then(|| {
            let mut dev = Ds1629::new();
            if let Some(w) = &config.ds1629_time {
                dev.set_time(ds1629_datetime(w));
            }
            attach(&mut bus, DS1629_ADDRESS, dev)
        });

        let r2025 = config.r2025_enabled.then(|| {
            let mut dev = R2025::new();
            if let Some(w) = &config.r2025_time {
                dev.set_time(r2025_datetime(w));
            }
            attach(&mut bus, R2025_ADDRESS, dev)
        });

        let lcd = config
            .lcd_enabled
            .then(|| attach(&mut bus, config.lcd_address, Hd44780Pcf8574::new()));

        let bmp280 = config
            .bmp280_enabled
            .then(|| attach(&mut bus, BMP280_ADDRESS, Bmp280::new()));

        let bme680 = config
            .bme680_enabled
            .then(|| attach(&mut bus, BME680_ADDRESS, Bme680::new()));

        let am2320 = config
            .am2320_enabled
            .then(|| attach(&mut bus, AM2320_ADDRESS, Am2320::new()));

        let fan = config
            .fan_enabled
            .then(|| attach(&mut bus, config.fan_address, Max31760::new()));

        let mut device_addresses = Vec::new();
        if pcf8574.is_some() {
            device_addresses.push(("pcf8574", config.pcf8574_address));
        }
        if eeprom.is_some() {
            device_addresses.push(("eeprom", config.eeprom_address));
        }
        if lm75.is_some() {
            device_addresses.push(("lm75", config.lm75_address));
        }
        if ltc2990.is_some() {
            device_addresses.push(("ltc2990", config.ltc2990_address));
        }
        if pcf8583.is_some() {
            device_addresses.push(("pcf8583", config.pcf8583_address));
        }
        if ds1307.is_some() {
            device_addresses.push(("ds1307", DS1307_ADDRESS));
        }
        if ds1629.is_some() {
            device_addresses.push(("ds1629", DS1629_ADDRESS));
        }
        if r2025.is_some() {
            device_addresses.push(("r2025", R2025_ADDRESS));
        }
        if lcd.is_some() {
            device_addresses.push(("lcd", config.lcd_address));
        }
        if bmp280.is_some() {
            device_addresses.push(("bmp280", BMP280_ADDRESS));
        }
        if bme680.is_some() {
            device_addresses.push(("bme680", BME680_ADDRESS));
        }
        if am2320.is_some() {
            device_addresses.push(("am2320", AM2320_ADDRESS));
        }
        if fan.is_some() {
            device_addresses.push(("fan", config.fan_address));
        }

        Self {
            pcf: Pcf8584::new(),
            bus,
            pcf8574,
            eeprom,
            lm75,
            ltc2990,
            pcf8583,
            ds1307,
            ds1629,
            r2025,
            lcd,
            bmp280,
            bme680,
            am2320,
            fan,
            device_addresses,
            lcd_columns: config.lcd_columns,
            lcd_last_lines: None,
        }
    }

    // -- Scenario-facing setters (docs/PLAN.md section 3.6). Each is a
    // no-op if the corresponding device isn't enabled, matching a real
    // scenario script targeting a board configuration that doesn't carry
    // that device -- not a bug to report, just nothing to do.

    pub fn set_ltc2990_tint(&self, celsius: f32) {
        if let Some(dev) = &self.ltc2990 {
            dev.borrow_mut().set_tint(celsius);
        }
    }
    pub fn set_ltc2990_v1(&self, volts: f32) {
        if let Some(dev) = &self.ltc2990 {
            dev.borrow_mut().set_v1(volts);
        }
    }
    pub fn set_ltc2990_v2(&self, volts: f32) {
        if let Some(dev) = &self.ltc2990 {
            dev.borrow_mut().set_v2(volts);
        }
    }
    pub fn set_ltc2990_external_temp(&self, celsius: f32) {
        if let Some(dev) = &self.ltc2990 {
            dev.borrow_mut().set_external_temp(celsius);
        }
    }
    pub fn set_ltc2990_vcc(&self, volts: f32) {
        if let Some(dev) = &self.ltc2990 {
            dev.borrow_mut().set_vcc(volts);
        }
    }
    pub fn set_lm75_celsius(&self, celsius: f32) {
        if let Some(dev) = &self.lm75 {
            dev.borrow_mut().set_celsius(celsius);
        }
    }
    pub fn set_pcf8583_time(&self, time: DateTime) {
        if let Some(dev) = &self.pcf8583 {
            dev.borrow_mut().set_time(time);
        }
    }
    pub fn set_ds1307_time(&self, time: ds1307::DateTime) {
        if let Some(dev) = &self.ds1307 {
            dev.borrow_mut().set_time(time);
        }
    }
    pub fn set_ds1629_time(&self, time: ds1629::DateTime) {
        if let Some(dev) = &self.ds1629 {
            dev.borrow_mut().set_time(time);
        }
    }
    pub fn set_r2025_time(&self, time: r2025::DateTime) {
        if let Some(dev) = &self.r2025 {
            dev.borrow_mut().set_time(time);
        }
    }
    pub fn set_bmp280_celsius(&self, celsius: f32) {
        if let Some(dev) = &self.bmp280 {
            dev.borrow_mut().set_celsius(celsius);
        }
    }
    pub fn set_bmp280_pressure_hpa(&self, hpa: f32) {
        if let Some(dev) = &self.bmp280 {
            dev.borrow_mut().set_pressure_hpa(hpa);
        }
    }
    pub fn set_bme680_celsius(&self, celsius: f32) {
        if let Some(dev) = &self.bme680 {
            dev.borrow_mut().set_celsius(celsius);
        }
    }
    pub fn set_bme680_pressure_hpa(&self, hpa: f32) {
        if let Some(dev) = &self.bme680 {
            dev.borrow_mut().set_pressure_hpa(hpa);
        }
    }
    pub fn set_bme680_humidity_percent(&self, percent: f32) {
        if let Some(dev) = &self.bme680 {
            dev.borrow_mut().set_humidity_percent(percent);
        }
    }
    pub fn set_am2320_celsius(&self, celsius: f32) {
        if let Some(dev) = &self.am2320 {
            dev.borrow_mut().set_celsius(celsius);
        }
    }
    pub fn set_am2320_humidity_percent(&self, percent: f32) {
        if let Some(dev) = &self.am2320 {
            dev.borrow_mut().set_humidity_percent(percent);
        }
    }
    pub fn set_fan_stuck(&self, stuck: bool) {
        if let Some(dev) = &self.fan {
            dev.borrow_mut().fan_mut().set_stuck(stuck);
        }
    }
    pub fn fan_duty(&self) -> Option<u8> {
        self.fan.as_ref().map(|d| d.borrow().fan().duty())
    }
    pub fn fan_rpm(&self) -> Option<u32> {
        self.fan.as_ref().map(|d| d.borrow().fan().rpm())
    }

    /// The LCD's current visible text, if it's enabled and its content
    /// has changed since the last call -- `None` both when there's no
    /// LCD and when there is one but nothing changed, so a caller can
    /// unconditionally call this every tick without distinguishing the
    /// two "nothing to report" cases.
    ///
    /// This is the "export" side of the LCD: `board.rs` stays host-ABI-
    /// agnostic (only `lib.rs` may call `host_log`, per this crate's
    /// top-level docs), so this method just computes and diffs the
    /// text; `lib.rs`'s `tick` export is what actually logs it.
    pub fn lcd_text_if_changed(&mut self) -> Option<[String; 2]> {
        let dev = self.lcd.as_ref()?.borrow();
        let lines = [dev.line(0, self.lcd_columns), dev.line(1, self.lcd_columns)];
        if self.lcd_last_lines.as_ref() == Some(&lines) {
            return None;
        }
        drop(dev);
        self.lcd_last_lines = Some(lines.clone());
        Some(lines)
    }

    /// Fault knob: "device unplugged" -- the given address stops
    /// acknowledging entirely (docs/PLAN.md section 3.4's "address NAK"
    /// fixture), regardless of which device (if any) is actually
    /// attached there.
    pub fn set_address_unplugged(&mut self, addr: u8, unplugged: bool) {
        self.bus.set_unplugged(addr, unplugged);
    }

    /// Same fault, targeted by device name instead of raw address (what
    /// a [`crate::scenario`] script actually writes). Returns `false` if
    /// no enabled device has that name -- a scenario targeting a device
    /// this board configuration doesn't carry, which the caller may want
    /// to log but shouldn't treat as fatal.
    pub fn set_device_unplugged(&mut self, name: &str, unplugged: bool) -> bool {
        match self.device_addresses.iter().find(|(n, _)| *n == name) {
            Some((_, addr)) => {
                self.bus.set_unplugged(*addr, unplugged);
                true
            }
            None => false,
        }
    }

    /// One byte of the board's address space. `byte_addr` is a Zorro
    /// byte address relative to the board base (0-based, not masked to
    /// the window size -- callers pass the raw offset the host gives
    /// them). See the module docs for the two decode facts this encodes.
    fn read_byte(&mut self, byte_addr: u32) -> u8 {
        if byte_addr & 1 != 0 {
            // Odd address: /LDS only, chip's data bus isn't wired there.
            return OPEN_BUS_BYTE;
        }
        let s1 = (byte_addr >> 1) & 1 != 0; // BA1 = Zorro A1
        self.pcf.read(s1, &mut self.bus)
    }

    fn write_byte(&mut self, byte_addr: u32, value: u8) {
        if byte_addr & 1 != 0 {
            return;
        }
        let s1 = (byte_addr >> 1) & 1 != 0;
        self.pcf.write(s1, value, &mut self.bus);
    }

    /// `size` is 1, 2, or 4, per the Copperline plugin ABI (docs/zorro.md);
    /// values are composed big-endian, right-aligned, matching the 68k's
    /// own byte ordering -- see docs/PLAN.md section 3.2.
    pub fn read(&mut self, off: u32, size: u32) -> u32 {
        let mut result: u32 = 0;
        for i in 0..size {
            let byte = self.read_byte(off.wrapping_add(i));
            result = (result << 8) | u32::from(byte);
        }
        result
    }

    pub fn write(&mut self, off: u32, size: u32, value: u32) {
        for i in 0..size {
            let shift = 8 * (size - 1 - i);
            let byte = ((value >> shift) & 0xFF) as u8;
            self.write_byte(off.wrapping_add(i), byte);
        }
    }

    pub fn tick(&mut self, cck: u32) {
        self.pcf.tick(cck, &mut self.bus);
        self.bus.tick(cck);
    }

    pub fn int2_asserted(&self) -> bool {
        self.pcf.int_asserted()
    }
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcf8584::CCK_PER_BYTE_PHASE;

    /// Control-register bits, duplicated from pcf8584's private `ctrl`
    /// module (not exported -- board tests drive the chip only through
    /// the board's own byte-level read/write, the same surface
    /// `i2c.library` uses).
    const PIN: u32 = 0x80;
    const ESO: u32 = 0x40;
    const STA: u32 = 0x04;
    const STO: u32 = 0x02;
    const ACK: u32 = 0x01;

    const PCF8574_ADDRESS: u8 = 0x20;

    #[test]
    fn wallclock_conversions_truncate_the_year_and_remap_the_weekday_per_device() {
        // 2026-08-16 is a Sunday (weekday_sun0() == 0).
        let w = crate::rtc_time::parse("2026-08-16 14:30:05").unwrap();

        let pcf = pcf8583_datetime(&w);
        assert_eq!(pcf.year_low2, (2026u32 % 4) as u8);
        assert_eq!(pcf.weekday, 0);
        assert_eq!((pcf.month, pcf.date, pcf.hour, pcf.minute, pcf.second), (8, 16, 14, 30, 5));

        let ds1307 = ds1307_datetime(&w);
        assert_eq!(ds1307.year, 26);
        assert_eq!(ds1307.weekday, 1, "DS1307 weekday is 1-7, Sunday=1");

        let ds1629 = ds1629_datetime(&w);
        assert_eq!(ds1629.year, 26);
        assert_eq!(ds1629.weekday, 1, "DS1629 weekday is 1-7, Sunday=1");

        let r2025 = r2025_datetime(&w);
        assert_eq!(r2025.year, 26);
        assert_eq!(r2025.weekday, 0, "R2025 weekday is 0-6, Sunday=0");
    }

    #[test]
    fn a_configured_initial_time_is_readable_over_the_bus() {
        let mut board = Board::with_config(BoardConfig {
            pcf8583_enabled: true,
            pcf8583_time: Some(crate::rtc_time::parse("2026-08-16 14:30:05").unwrap()),
            ..BoardConfig::default()
        });
        let pcf8583_address = BoardConfig::default().pcf8583_address;

        board.write(2, 1, PIN | ESO | ACK);
        board.write(0, 1, u32::from(pcf8583_address) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        board.write(0, 1, 0x02); // pointer -> seconds register
        board.tick(CCK_PER_BYTE_PHASE);
        board.write(2, 1, PIN | ESO | STO | ACK);

        board.write(0, 1, (u32::from(pcf8583_address) << 1) | 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        let _dummy = board.read(0, 1);
        board.tick(CCK_PER_BYTE_PHASE);
        let seconds = board.read(0, 1);
        board.write(2, 1, PIN | ESO | STO | ACK);

        assert_eq!(seconds, 0x05, "seconds BCD for :05, read back from a configured initial time");
    }

    #[test]
    fn ltc2990_and_fan_addresses_are_configurable() {
        let board = Board::with_config(BoardConfig {
            ltc2990_address: 0x4D, // Fanny Card wiring (0x9A 8-bit)
            fan_address: 0x51,     // alternate Fanny strapping (0xA2 8-bit)
            ..BoardConfig::default()
        });
        assert!(board.device_addresses.contains(&("ltc2990", 0x4D)));
        assert!(board.device_addresses.contains(&("fan", 0x51)));
    }

    #[test]
    fn moving_pcf8583_off_fans_default_address_lets_both_coexist() {
        // pcf8583_address defaults to the same address as fan_address
        // (module docs on both fields) -- a real board could never
        // populate both there at once either, so this is the escape
        // hatch, not a bug to route around silently.
        let mut board = Board::with_config(BoardConfig {
            pcf8583_enabled: true,
            pcf8583_address: 0x51, // moved off fan's default (0x50)
            ..BoardConfig::default()
        });

        // The fan is still reachable at its own (default) address.
        board.write(2, 1, PIN | ESO | ACK);
        board.write(0, 1, u32::from(MAX31760_ADDRESS) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        let status = board.read(2, 1);
        assert_eq!(status & 0x08, 0, "MAX31760 should ACK its own address once pcf8583 has moved off it");
        board.write(2, 1, PIN | ESO | STO | ACK);
    }

    #[test]
    fn even_word_offset_0_reaches_the_a0_area_and_offset_2_reaches_s1() {
        let mut board = Board::new();

        // S1 lives at offset 2: a plain status read should show PIN=1,
        // BB=1 (reset defaults) in the high byte of a word read.
        let s1 = board.read(2, 2);
        assert_eq!(s1, 0x81FF, "S1 in the high byte, open bus (0xFF) in the low byte");

        // Offset 0 reaches the A0 area (S0' at reset, since ESO=0):
        // writing and reading back an own-address value should round-trip.
        board.write(0, 1, 0x55);
        assert_eq!(board.read(0, 1), 0x55);
    }

    #[test]
    fn odd_byte_addresses_never_reach_the_chip() {
        let mut board = Board::new();
        board.write(0, 1, 0x55); // S0' = 0x55, via the even (UDS) address
        assert_eq!(board.read(1, 1), OPEN_BUS_BYTE as u32, "odd address is /LDS-only: open bus");

        board.write(1, 1, 0xAB); // should be silently dropped
        assert_eq!(board.read(0, 1), 0x55, "the S0' write above must be unaffected");
    }

    #[test]
    fn register_select_mirrors_every_four_bytes_across_the_window() {
        let mut board = Board::new();
        // Per the module docs: only A1 is decoded, so offsets 4/6 alias
        // offsets 0/2.
        board.write(4, 1, 0x33); // aliases offset 0 (A0 area)
        assert_eq!(board.read(0, 1), 0x33);
        assert_eq!(board.read(4, 1), 0x33);

        let s1_direct = board.read(2, 1);
        let s1_aliased = board.read(6, 1);
        assert_eq!(s1_direct, s1_aliased);
    }

    #[test]
    fn size_four_access_composes_both_registers() {
        let mut board = Board::new();
        board.write(0, 1, 0x77); // S0'
        // A long read at offset 0 covers bytes 0..4: A0 area (even),
        // open bus (odd), S1 (even), open bus (odd).
        let long_read = board.read(0, 4);
        assert_eq!(long_read, 0x77_FF_81_FF, "A0-area byte, open bus, S1 byte, open bus");
    }

    #[test]
    fn a_full_master_transmit_transaction_through_the_board_window_acks() {
        let mut board = Board::new();

        // Select S0 for transfers.
        board.write(2, 1, PIN | ESO | ACK);
        // Address+W for the PCF8574 at 0x20.
        board.write(0, 1, u32::from(PCF8574_ADDRESS) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);

        let status_after_addr = board.read(2, 1);
        assert_eq!(status_after_addr & 0x08, 0, "PCF8574 should ACK its address (LRB=0)");

        // Write a data byte (the new output-latch value) and complete it.
        board.write(0, 1, 0b1010_0101);
        board.tick(CCK_PER_BYTE_PHASE);
        let status_after_data = board.read(2, 1);
        assert_eq!(status_after_data & 0x08, 0, "PCF8574 should ACK the data byte too");

        board.write(2, 1, PIN | ESO | STO | ACK);

        // Read it back through a fresh master-receive transaction.
        board.write(0, 1, (u32::from(PCF8574_ADDRESS) << 1) | 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        let _dummy = board.read(0, 1);
        board.tick(CCK_PER_BYTE_PHASE);
        let readback = board.read(0, 1);
        board.write(2, 1, PIN | ESO | STO | ACK);
        assert_eq!(readback, 0b1010_0101, "should read back exactly what was written");
    }

    #[test]
    fn int2_asserts_and_deasserts_following_the_pcf8584_pin_bit() {
        let mut board = Board::new();
        assert!(!board.int2_asserted());

        board.write(2, 1, PIN | ESO | ACK);
        board.write(0, 1, u32::from(PCF8574_ADDRESS) << 1);
        // ENI bit is 0x08; include it alongside STA this time.
        board.write(2, 1, PIN | ESO | 0x08 | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);

        assert!(board.int2_asserted(), "PIN active + ENI set should raise INT2");

        board.write(2, 1, PIN | ESO | STO | ACK);
        assert!(!board.int2_asserted(), "STOP should deassert INT2");
    }

    #[test]
    fn default_config_enables_pcf8574_ltc2990_and_fan_but_not_the_opt_in_devices() {
        let board = Board::new();
        assert!(board.pcf8574.is_some());
        assert!(board.ltc2990.is_some());
        assert!(board.fan.is_some());
        assert!(board.eeprom.is_none());
        assert!(board.lm75.is_none());
        assert!(board.pcf8583.is_none());
        assert!(board.ds1307.is_none());
        assert!(board.ds1629.is_none());
        assert!(board.r2025.is_none());
        assert!(board.lcd.is_none());
        assert!(board.bmp280.is_none());
        assert!(board.bme680.is_none());
        assert!(board.am2320.is_none());
    }

    #[test]
    fn lcd_text_if_changed_reports_only_on_change_and_none_when_disabled() {
        let mut disabled = Board::new();
        assert_eq!(disabled.lcd_text_if_changed(), None);

        let mut board = Board::with_config(BoardConfig {
            lcd_enabled: true,
            ..BoardConfig::default()
        });
        let lcd_address = HD44780_PCF8574_DEFAULT_ADDRESS;

        // First call always reports (blank -> blank is still a change
        // from "no report yet").
        assert_eq!(board.lcd_text_if_changed(), Some([" ".repeat(16), " ".repeat(16)]));
        // Nothing changed since: no report.
        assert_eq!(board.lcd_text_if_changed(), None);

        // Drive "Hi" onto the display over the bus, the same way a real
        // guest driver would: select S0, address+W the LCD, then send
        // RS=1/EN-pulsed nibbles for 'H' and 'i'.
        board.write(2, 1, PIN | ESO | ACK);
        board.write(0, 1, u32::from(lcd_address) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        for &ch in b"Hi" {
            for nibble in [ch >> 4, ch & 0x0F] {
                let base = 0x01 | (u32::from(nibble) << 4) | 0x08; // RS=1, backlight=1
                board.write(0, 1, base | 0x04); // EN=1
                board.tick(CCK_PER_BYTE_PHASE);
                board.write(0, 1, base); // EN=0 -- falling edge latches the nibble
                board.tick(CCK_PER_BYTE_PHASE);
            }
        }
        board.write(2, 1, PIN | ESO | STO | ACK);

        let mut expected_row0 = "Hi".to_string();
        expected_row0.push_str(&" ".repeat(14));
        assert_eq!(board.lcd_text_if_changed(), Some([expected_row0, " ".repeat(16)]));
        assert_eq!(board.lcd_text_if_changed(), None, "unchanged since the last report");
    }

    #[test]
    fn scenario_setters_are_a_no_op_when_the_device_is_disabled() {
        let board = Board::with_config(BoardConfig {
            ltc2990_enabled: false,
            ..BoardConfig::default()
        });
        // Should not panic even though there's no LTC2990 to set.
        board.set_ltc2990_tint(50.0);
    }

    #[test]
    fn unplugged_fault_makes_a_configured_device_stop_acking() {
        let mut board = Board::new();
        board.write(2, 1, PIN | ESO | ACK);

        board.set_address_unplugged(PCF8574_ADDRESS, true);
        board.write(0, 1, u32::from(PCF8574_ADDRESS) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        let status = board.read(2, 1);
        assert_ne!(status & 0x08, 0, "unplugged device should NAK its address");

        board.write(2, 1, PIN | ESO | STO | ACK);
        board.set_address_unplugged(PCF8574_ADDRESS, false);
        board.write(0, 1, u32::from(PCF8574_ADDRESS) << 1);
        board.write(2, 1, PIN | ESO | STA | ACK);
        board.tick(CCK_PER_BYTE_PHASE);
        let status = board.read(2, 1);
        assert_eq!(status & 0x08, 0, "re-plugging should restore the ACK");
    }
}
