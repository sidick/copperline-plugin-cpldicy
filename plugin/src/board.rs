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

use crate::devices::eeprom24::Eeprom24;
use crate::devices::lm75::Lm75;
use crate::devices::ltc2990::Ltc2990;
use crate::devices::pcf8583::{DateTime, Pcf8583};
use crate::devices::pcf8574::Pcf8574;
use crate::fan::{Max31760, MAX31760_ADDRESS};
use crate::i2c::{I2cBusEngine, SharedDevice};
use crate::pcf8584::Pcf8584;
use std::cell::RefCell;
use std::rc::Rc;

/// A read from an address the board doesn't drive: open bus.
const OPEN_BUS_BYTE: u8 = 0xFF;

/// Per-device enable/address configuration (docs/PLAN.md section 3.7's
/// manifest `[config]` schema). PCF8574/LTC2990/the fan controller
/// default on -- the GPIO expander because Phase 1's `i2c.library` gate
/// depends on it being there out of the box, LTC2990/fan because they're
/// the real board's own authentic residents (docs/board-facts.md §5-6).
/// EEPROM/LM75/PCF8583 are opt-in teaching devices, off by default to
/// keep an out-of-the-box bus scan uncluttered.
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
    pub fan_enabled: bool,
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
            ltc2990_address: 0x4C, // docs/board-facts.md §6
            pcf8583_enabled: false,
            pcf8583_address: 0x51,
            fan_enabled: true,
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
    fan: Option<Rc<RefCell<Max31760>>>,
    /// Name -> configured address, for enabled devices only -- lets
    /// [`crate::scenario`] target a fault fixture ("unplug the sensor")
    /// by the same device name a scenario script uses, without needing
    /// to know raw I2C addresses.
    device_addresses: Vec<(&'static str, u8)>,
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

        let pcf8583 = config
            .pcf8583_enabled
            .then(|| attach(&mut bus, config.pcf8583_address, Pcf8583::new()));

        let fan = config
            .fan_enabled
            .then(|| attach(&mut bus, MAX31760_ADDRESS, Max31760::new()));

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
        if fan.is_some() {
            device_addresses.push(("fan", MAX31760_ADDRESS));
        }

        Self {
            pcf: Pcf8584::new(),
            bus,
            pcf8574,
            eeprom,
            lm75,
            ltc2990,
            pcf8583,
            fan,
            device_addresses,
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
