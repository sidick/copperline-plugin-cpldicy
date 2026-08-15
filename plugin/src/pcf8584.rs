//! PCF8584 byte-oriented I2C bus controller: the chip CPLDIcy's board
//! window maps directly onto (docs/board-facts.md section 4). This is a
//! board-agnostic model of the chip itself -- it knows nothing about
//! Zorro addresses, byte lanes, or CPLDIcy's specific wiring (that's
//! [`crate::board`]); it only knows the two host-visible register
//! addresses (S1, and the ES1/ES2-muxed "A0 area" the datasheet calls
//! S0/S0'/S2/S3) and drives an [`I2cBus`] through them.
//!
//! Bus timing is virtual: each addressing/data phase takes
//! [`CCK_PER_BYTE_PHASE`] emulated cycles to "complete" rather than
//! modeling real SCL edges, matching docs/PLAN.md section 3.2's "one bus
//! phase per N ccks" plan -- enough to make PIN/BB status genuinely
//! observable in the right order (a poll loop that reads status
//! immediately after triggering a START *will* see PIN=1/busy at least
//! once), without emulating individual clock edges.
//!
//! Several details here are call-outs from docs/board-facts.md section 4
//! marked as needing oracle verification (unmodified `i2c.library`
//! actually detecting and driving this model) before being trusted at
//! face value -- see the doc comments on [`Pcf8584::soft_reset`] and the
//! PIN-bit-alone write path in particular. Getting `i2c.library` to
//! detect the board (docs/PLAN.md Phase 1 gate) is expected to surface
//! quirks; when it does, the fix belongs here with a unit test, not as a
//! one-off patch.

use crate::i2c::I2cBus;

/// Emulated bus cycles for one addressing or data phase (9 SCL clocks:
/// 8 data bits + ACK). Deliberately small and fixed rather than derived
/// from the S2 clock register's SCL-frequency bits -- the real board's
/// own README admits its CPLD-mediated PCF8584 timing isn't fully
/// faithful to a real chip's, so there is no real "ground truth" timing
/// to match here; this just needs to be nonzero so poll loops observe
/// PIN=1 (busy) at least once, and small enough that Copperline's fuel
/// budget and the guest's real-time interrupt timeouts are never at risk.
pub const CCK_PER_BYTE_PHASE: u32 = 200;

// Control register S1 (write) bit positions -- docs/board-facts.md §4.
mod ctrl {
    pub const PIN: u8 = 0x80;
    pub const ESO: u8 = 0x40;
    pub const ES1: u8 = 0x20;
    pub const ES2: u8 = 0x10;
    pub const ENI: u8 = 0x08;
    pub const STA: u8 = 0x04;
    pub const STO: u8 = 0x02;
    pub const ACK: u8 = 0x01;
}

// Status register S1 (read) bit positions -- docs/board-facts.md §4.
mod status {
    pub const PIN: u8 = 0x80;
    pub const STS: u8 = 0x20;
    pub const BER: u8 = 0x10;
    pub const LRB_AD0: u8 = 0x08;
    pub const AAS: u8 = 0x04;
    pub const LAB: u8 = 0x02;
    pub const BB: u8 = 0x01;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// No transaction in progress (or acting as slave-receiver, which
    /// Phase 1 doesn't implement -- see the module docs).
    Idle,
    MasterTx,
    MasterRx,
}

/// What's "on the wire" right now, counted down by [`Pcf8584::tick`].
#[derive(Clone, Copy, Debug)]
enum Pending {
    /// Clocking out the address + R/W byte already latched in the S0
    /// shift register when STA was written.
    Address { addr7: u8, read: bool },
    /// Clocking out a data byte (master-transmit) already latched in S0.
    TxByte,
    /// Clocking in a data byte (master-receive), armed by an S0 read.
    /// `ack` is the ACK/NACK the *master* will send for this byte,
    /// sampled from the control register at arm time (see
    /// [`Pcf8584::read_a0_area`]'s docs for why it must be sampled then,
    /// not at completion).
    RxByte { ack: bool },
}

pub struct Pcf8584 {
    // Control latch (S1 write side).
    eso: bool,
    es1: bool,
    es2: bool,
    eni: bool,
    ack: bool,

    // Status latch (S1 read side). `pin` follows the datasheet's odd
    // polarity throughout this file: `false` = PIN bit 0 = "active"
    // (interrupt pending / transaction in flight); `true` = PIN bit 1 =
    // "inactive" (idle, ready for the next register access).
    pin: bool,
    sts: bool,
    ber: bool,
    lrb_ad0: bool,
    aas: bool,
    lab: bool,
    /// Datasheet polarity: `true` = bus free, `false` = busy.
    bb: bool,

    // The "A0 area" registers, muxed by (es1, es2) while eso == false;
    // always S0 (data) while eso == true.
    s0_shift: u8,    // write side: what's queued to send next.
    s0_read_buf: u8, // read side: last byte pulled in from the bus.
    s0_own_addr: u8, // S0': own 7-bit address (bits 7..1), init only.
    s2_clock: u8,    // clock register: no documented reset default.
    s3_vector: u8,   // interrupt vector register: resets to 0x00.

    mode: Mode,
    pending: Option<Pending>,
    ticks_remaining: u32,
}

impl Pcf8584 {
    pub fn new() -> Self {
        let mut chip = Self {
            eso: false,
            es1: false,
            es2: false,
            eni: false,
            ack: false,
            pin: true,
            sts: false,
            ber: false,
            lrb_ad0: false,
            aas: false,
            lab: false,
            bb: true,
            s0_shift: 0,
            s0_read_buf: 0,
            s0_own_addr: 0,
            s2_clock: 0,
            s3_vector: 0,
            mode: Mode::Idle,
            pending: None,
            ticks_remaining: 0,
        };
        chip.reset();
        chip
    }

    /// Hardware RESET pin behavior (docs/board-facts.md §4 "Reset
    /// defaults"): all S1 flags clear except PIN and BB, which set;
    /// S0'/S3 clear; S2 is left as-is (undocumented default -- firmware
    /// is expected to always program it, so there's nothing meaningful
    /// to reset it to).
    pub fn reset(&mut self) {
        self.eso = false;
        self.es1 = false;
        self.es2 = false;
        self.eni = false;
        self.ack = false;
        self.pin = true;
        self.sts = false;
        self.ber = false;
        self.lrb_ad0 = false;
        self.aas = false;
        self.lab = false;
        self.bb = true;
        self.s0_own_addr = 0;
        self.s3_vector = 0;
        self.mode = Mode::Idle;
        self.pending = None;
        self.ticks_remaining = 0;
    }

    /// True exactly when the chip's INT pin would be asserted: an active
    /// (unacknowledged) interrupt condition (`pin == false`) with
    /// interrupt output enabled. CPLDIcy wires this straight to Zorro
    /// INT2 with no CPLD-side logic in between (docs/board-facts.md §3).
    pub fn int_asserted(&self) -> bool {
        !self.pin && self.eni
    }

    /// Dispatch for the board's two register addresses (docs/board-facts.md
    /// §2): `false` = the ES1/ES2-muxed "A0 area", `true` = S1.
    pub fn read(&mut self, s1: bool, bus: &mut dyn I2cBus) -> u8 {
        if s1 {
            self.read_s1()
        } else {
            self.read_a0_area(bus)
        }
    }

    pub fn write(&mut self, s1: bool, value: u8, bus: &mut dyn I2cBus) {
        if s1 {
            self.write_s1(value, bus);
        } else {
            self.write_a0_area(value, bus);
        }
    }

    fn read_s1(&self) -> u8 {
        let mut v = 0u8;
        if self.pin {
            v |= status::PIN;
        }
        if self.sts {
            v |= status::STS;
        }
        if self.ber {
            v |= status::BER;
        }
        if self.lrb_ad0 {
            v |= status::LRB_AD0;
        }
        if self.aas {
            v |= status::AAS;
        }
        if self.lab {
            v |= status::LAB;
        }
        if self.bb {
            v |= status::BB;
        }
        v
    }

    /// Writing the control register. `byte == ctrl::PIN` alone (0x80, no
    /// other bits) is the datasheet's documented software-reset trick,
    /// used by `i2c.library`'s init probe (docs/board-facts.md §4's
    /// "Detection/compatibility requirements") to resynchronize the
    /// chip. **Oracle-unverified**: the exact post-trick readback the
    /// real chip (and CPLDIcy's CPLD-mediated approximation of it)
    /// produces isn't nailed down by the datasheet text alone -- see
    /// docs/board-facts.md §4's driver-probe discussion. This
    /// implementation treats it as clearing the fault-ish status bits
    /// (BER/STS/AAS/LAB) and asserting PIN, leaving BB untouched, which
    /// is the most defensible reading of "resets all status bits to 0"
    /// without contradicting the separate hardware-RESET-pin default of
    /// BB=1. If the oracle pass (docs/PLAN.md Phase 1 step 5) shows
    /// `i2c.library` expects something else, fix it here with a test
    /// that pins the corrected behavior down.
    fn write_s1(&mut self, byte: u8, bus: &mut dyn I2cBus) {
        if byte == ctrl::PIN {
            self.ber = false;
            self.sts = false;
            self.aas = false;
            self.lab = false;
            self.pin = true;
            return;
        }

        self.eso = byte & ctrl::ESO != 0;
        self.es1 = byte & ctrl::ES1 != 0;
        self.es2 = byte & ctrl::ES2 != 0;
        self.eni = byte & ctrl::ENI != 0;
        self.ack = byte & ctrl::ACK != 0;

        let sta = byte & ctrl::STA != 0;
        let sto = byte & ctrl::STO != 0;
        match (sta, sto) {
            (true, false) => self.handle_start(bus),
            (false, true) => self.handle_stop(bus),
            (true, true) => {
                // Data chaining: STOP then START again, without ever
                // releasing the bus to another master (docs/board-facts.md
                // §4's STA/STO table). Sequencing the two handlers back to
                // back models this well enough for a single-master
                // emulation with no real electrical bus to hold.
                self.handle_stop(bus);
                self.handle_start(bus);
            }
            (false, false) => {} // NOP
        }
    }

    fn handle_start(&mut self, _bus: &mut dyn I2cBus) {
        // "Writing STA=1 sets PIN=1 (inactive) immediately" -- but the
        // actual address-phase completion (and PIN going active again)
        // happens on the next `tick`, once CCK_PER_BYTE_PHASE cycles have
        // elapsed, matching the datasheet's "poll PIN until it clears"
        // sequencing (docs/board-facts.md §4).
        self.bb = false;
        self.pin = true;
        let byte = self.s0_shift;
        let addr7 = byte >> 1;
        let read = byte & 1 != 0;
        self.pending = Some(Pending::Address { addr7, read });
        self.ticks_remaining = CCK_PER_BYTE_PHASE;
    }

    fn handle_stop(&mut self, bus: &mut dyn I2cBus) {
        bus.stop();
        self.bb = true;
        self.pin = true;
        self.mode = Mode::Idle;
        self.pending = None;
        self.ticks_remaining = 0;
    }

    /// The ES1/ES2-muxed "A0 area": S0 while `eso`, else S0'/S3/S2 by
    /// (es1, es2) -- docs/board-facts.md §4's register table.
    fn read_a0_area(&mut self, bus: &mut dyn I2cBus) -> u8 {
        if self.eso {
            self.read_s0(bus)
        } else {
            match (self.es1, self.es2) {
                (false, false) => self.s0_own_addr,
                (false, true) => self.s3_vector,
                (true, false) => self.s2_clock,
                (true, true) => 0xFF, // undefined combination
            }
        }
    }

    fn write_a0_area(&mut self, value: u8, bus: &mut dyn I2cBus) {
        if self.eso {
            self.write_s0(value, bus);
        } else {
            match (self.es1, self.es2) {
                (false, false) => self.s0_own_addr = value,
                (false, true) => self.s3_vector = value,
                (true, false) => self.s2_clock = value,
                (true, true) => {} // undefined combination: ignore
            }
        }
    }

    /// Reading S0. In master-receive mode this both returns the
    /// previously-clocked-in byte *and* (if the chip is currently
    /// "at rest", i.e. `pin == false` from a just-completed phase) arms
    /// clocking-in of the next one -- the datasheet's dummy-read
    /// pipeline (docs/board-facts.md §4): the very first such read after
    /// the address phase returns a stale/undefined byte and is expected
    /// to be discarded by firmware, while every read after that returns
    /// the previous real byte. The ACK/NACK the master will send for the
    /// byte being armed is sampled from `self.ack` *now*, matching
    /// `i2c.library`'s documented pattern of clearing ACK before the read
    /// that arms the final byte (docs/board-facts.md §4's master-receive
    /// sequence, step "before the last byte, clear ACK").
    fn read_s0(&mut self, _bus: &mut dyn I2cBus) -> u8 {
        let value = self.s0_read_buf;
        if self.mode == Mode::MasterRx && !self.pin {
            self.pin = true;
            self.pending = Some(Pending::RxByte { ack: self.ack });
            self.ticks_remaining = CCK_PER_BYTE_PHASE;
        }
        value
    }

    /// Writing S0. Before a START, this just preloads the shift register
    /// (the address+R/W byte `handle_start` will send) with no bus
    /// activity -- `mode` is still `Idle` at that point. In
    /// master-transmit mode, a write both preloads the byte and
    /// immediately begins clocking it out.
    fn write_s0(&mut self, byte: u8, _bus: &mut dyn I2cBus) {
        self.s0_shift = byte;
        if self.mode == Mode::MasterTx {
            self.pin = true;
            self.pending = Some(Pending::TxByte);
            self.ticks_remaining = CCK_PER_BYTE_PHASE;
        }
    }

    pub fn tick(&mut self, cck: u32, bus: &mut dyn I2cBus) {
        if self.pending.is_none() {
            return;
        }
        if cck >= self.ticks_remaining {
            self.ticks_remaining = 0;
            self.complete_pending(bus);
        } else {
            self.ticks_remaining -= cck;
        }
    }

    fn complete_pending(&mut self, bus: &mut dyn I2cBus) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            Pending::Address { addr7, read } => {
                let acked = bus.start(addr7, read);
                self.lrb_ad0 = !acked;
                self.aas = false; // master mode: never addressed as slave
                self.pin = false;
                self.mode = if read { Mode::MasterRx } else { Mode::MasterTx };
            }
            Pending::TxByte => {
                let acked = bus.write(self.s0_shift);
                self.lrb_ad0 = !acked;
                self.pin = false;
            }
            Pending::RxByte { ack } => {
                let byte = bus.read(ack);
                self.s0_read_buf = byte;
                self.lrb_ad0 = !ack;
                self.pin = false;
            }
        }
    }
}

impl Default for Pcf8584 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::{I2cBusEngine, I2cDevice};

    /// A device that ACKs everything and echoes a fixed byte, so tests can
    /// drive the full master-tx/master-rx sequences without needing a
    /// real device model.
    struct Stub {
        read_byte: u8,
    }
    impl I2cDevice for Stub {
        fn start(&mut self, _read: bool) -> bool {
            true
        }
        fn write(&mut self, _byte: u8) -> bool {
            true
        }
        fn read(&mut self, _master_will_ack: bool) -> u8 {
            self.read_byte
        }
        fn stop(&mut self) {}
    }

    fn bus_with_device(addr7: u8, read_byte: u8) -> I2cBusEngine {
        let mut bus = I2cBusEngine::new();
        bus.attach(addr7, Box::new(Stub { read_byte }));
        bus
    }

    use std::boxed::Box;

    #[test]
    fn reset_matches_documented_defaults() {
        let chip = Pcf8584::new();
        assert_eq!(chip.read_s1(), status::PIN | status::BB);
    }

    #[test]
    fn pin_alone_write_is_the_soft_reset_trick_and_reads_back_pin_only_low7_zero() {
        let mut chip = Pcf8584::new();
        let mut bus = I2cBusEngine::new();
        // Poison some status bits first so the trick has something to clear.
        chip.ber = true;
        chip.sts = true;
        chip.aas = true;
        chip.lab = true;

        chip.write(true, ctrl::PIN, &mut bus);

        let status = chip.read_s1();
        assert_eq!(status & 0x7F, status::BB, "low 7 bits should read back with only BB possibly set (bus wasn't started)");
        assert_ne!(status & status::PIN, 0, "PIN should read back set (inactive)");
    }

    #[test]
    fn register_mux_reaches_own_address_vector_and_clock_registers() {
        let mut chip = Pcf8584::new();
        let mut bus = I2cBusEngine::new();

        // ESO=0, ES1=0, ES2=0 -> S0' (own address).
        chip.write(true, 0x00, &mut bus); // clears eso/es1/es2 via a plain NOP write
        chip.write(false, 0xAA, &mut bus);
        assert_eq!(chip.read(false, &mut bus), 0xAA);

        // ESO=0, ES1=0, ES2=1 -> S3 (interrupt vector).
        chip.write(true, ctrl::ES2, &mut bus);
        chip.write(false, 0x0F, &mut bus);
        assert_eq!(chip.read(false, &mut bus), 0x0F);

        // ESO=0, ES1=1, ES2=0 -> S2 (clock register).
        chip.write(true, ctrl::ES1, &mut bus);
        chip.write(false, 0x1C, &mut bus);
        assert_eq!(chip.read(false, &mut bus), 0x1C);

        // Values are latched independently -- switching back to S0'
        // shouldn't have been disturbed by writing S2/S3.
        chip.write(true, 0x00, &mut bus);
        assert_eq!(chip.read(false, &mut bus), 0xAA);
    }

    #[test]
    fn master_transmit_full_sequence_acks_and_completes() {
        let mut bus = bus_with_device(0x20, 0);
        let mut chip = Pcf8584::new();

        // Poll BB: should read free before anything starts.
        assert_ne!(chip.read_s1() & status::BB, 0);

        // Init: select S0 for transfers (ESO=1) -- matches i2c.library's
        // own init sequence (docs/board-facts.md §4), and is required
        // before the address write below reaches the shift register
        // rather than the ES1/ES2-muxed S0'/S2/S3 registers.
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::ACK, &mut bus);

        // Write address+W (0x20<<1 | 0) to S0, then START.
        chip.write(false, 0x20 << 1, &mut bus);
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STA | ctrl::ACK, &mut bus);

        // Immediately after the START write, PIN should read inactive... no
        // wait: PIN=1 means inactive per this file's polarity note, and the
        // datasheet says STA sets PIN=1 immediately. The "poll PIN==0" loop
        // firmware runs is polling for it to become *active* (0) again once
        // the byte finishes -- so right after the write we expect PIN=1
        // still (transaction not yet complete).
        assert_ne!(chip.read_s1() & status::PIN, 0);
        assert_eq!(chip.read_s1() & status::BB, 0, "bus should read busy once STA is issued");

        // Advance past the address phase.
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        let status_after_addr = chip.read_s1();
        assert_eq!(status_after_addr & status::PIN, 0, "PIN should be active (0) once the address phase completes");
        assert_eq!(status_after_addr & status::LRB_AD0, 0, "LRB should read ACK'd (0)");

        // Send one data byte.
        chip.write(false, 0x42, &mut bus);
        assert_ne!(chip.read_s1() & status::PIN, 0, "writing S0 should re-arm PIN=1 (inactive) while clocking");
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        assert_eq!(chip.read_s1() & status::PIN, 0, "PIN should go active again once the byte finishes");
        assert_eq!(chip.read_s1() & status::LRB_AD0, 0, "byte should have been ACK'd");

        // STOP.
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STO | ctrl::ACK, &mut bus);
        let final_status = chip.read_s1();
        assert_ne!(final_status & status::BB, 0, "bus should read free after STOP");
        assert_ne!(final_status & status::PIN, 0, "PIN should read inactive after STOP");
    }

    #[test]
    fn master_transmit_to_unaddressed_device_naks() {
        let mut bus = I2cBusEngine::new(); // nothing attached
        let mut chip = Pcf8584::new();

        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::ACK, &mut bus);
        chip.write(false, 0x20 << 1, &mut bus);
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STA | ctrl::ACK, &mut bus);
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);

        assert_ne!(chip.read_s1() & status::LRB_AD0, 0, "an empty bus should NAK the address");
    }

    #[test]
    fn master_receive_dummy_read_then_real_bytes() {
        let mut bus = bus_with_device(0x20, 0x99);
        let mut chip = Pcf8584::new();

        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::ACK, &mut bus);

        // Address+R.
        chip.write(false, (0x20 << 1) | 1, &mut bus);
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STA | ctrl::ACK, &mut bus);
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        assert_eq!(chip.read_s1() & status::PIN, 0, "address phase should have completed");

        // Dummy read: arms the first real byte, returns stale content.
        let dummy = chip.read(false, &mut bus);
        let _ = dummy; // datasheet says firmware discards this value
        assert_ne!(chip.read_s1() & status::PIN, 0, "dummy read should re-arm PIN=1 while clocking the first byte");

        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        assert_eq!(chip.read_s1() & status::PIN, 0, "first real byte should be ready");

        // Real read: returns the first byte, arms the second (which we'll
        // NACK by clearing ACK first, as if this were the last one wanted).
        let first = chip.read(false, &mut bus);
        assert_eq!(first, 0x99);

        chip.write(true, ctrl::ESO, &mut bus); // clear ACK bit before the arming read
        let _ = chip.read(false, &mut bus); // arms the final byte with NACK
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);

        // STOP, then fetch the final buffered byte.
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STO | ctrl::ACK, &mut bus);
        let last = chip.read(false, &mut bus);
        assert_eq!(last, 0x99);
    }

    #[test]
    fn interrupt_asserts_only_when_eni_set_and_pin_active() {
        let mut bus = bus_with_device(0x20, 0);
        let mut chip = Pcf8584::new();
        assert!(!chip.int_asserted(), "should be deasserted at reset");

        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::ACK, &mut bus);
        chip.write(false, 0x20 << 1, &mut bus);
        // START with ENI clear: no interrupt even once the phase completes.
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STA | ctrl::ACK, &mut bus);
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        assert!(!chip.int_asserted(), "ENI was never set");

        // Send a byte with ENI now set.
        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::ENI | ctrl::ACK, &mut bus); // NOP, just latches ENI
        chip.write(false, 0x11, &mut bus);
        chip.tick(CCK_PER_BYTE_PHASE, &mut bus);
        assert!(chip.int_asserted(), "PIN active + ENI set should assert INT2");

        // Reading status doesn't clear PIN by itself (only an S0
        // access/STOP does) -- confirm INT stays asserted across a status
        // read (a real driver clears it by continuing the transaction).
        let _ = chip.read_s1();
        assert!(chip.int_asserted());

        chip.write(true, ctrl::PIN | ctrl::ESO | ctrl::STO | ctrl::ACK, &mut bus);
        assert!(!chip.int_asserted(), "STOP should deassert (PIN inactive again)");
    }
}
