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

use crate::devices::pcf8574::Pcf8574;
use crate::i2c::I2cBusEngine;
use crate::pcf8584::Pcf8584;

/// Standard PCF8574 default address (A0-A2 grounded). Not an authentic
/// CPLDIcy resident -- it's the "blink an LED / read a button" sample
/// device docs/PLAN.md section 3.4 calls for. Hardcoded for Phase 1;
/// Phase 2's config surface makes this configurable/optional.
const PCF8574_ADDRESS: u8 = 0x20;

/// A read from an address the board doesn't drive: open bus.
const OPEN_BUS_BYTE: u8 = 0xFF;

pub struct Board {
    pcf: Pcf8584,
    bus: I2cBusEngine,
}

impl Board {
    pub fn new() -> Self {
        let mut bus = I2cBusEngine::new();
        bus.attach(PCF8574_ADDRESS, Box::new(Pcf8574::new()));
        Self {
            pcf: Pcf8584::new(),
            bus,
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
}
