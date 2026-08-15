//! The virtual I2C bus: a registry of [`I2cDevice`]s addressed by their
//! 7-bit slave address, and the [`I2cBus`] trait [`crate::pcf8584::Pcf8584`]
//! drives transactions through. Kept separate from the PCF8584 controller
//! model so the controller's state machine can be unit-tested against a
//! fake bus, and so devices can be unit-tested in isolation against their
//! own datasheet register maps without any PCF8584/board machinery.

/// One I2C slave device. Mirrors the wire-level protocol directly: an
/// address phase, then a run of byte transfers in one direction, then a
/// stop. A device never sees the controller's register-level details
/// (PIN, ESO, ...) -- only these four calls.
pub trait I2cDevice {
    /// Address phase: the master has selected this device. `read` is the
    /// R/W bit from the address byte (true = master wants to read).
    /// Returns whether the device acknowledges (true = ACK).
    fn start(&mut self, read: bool) -> bool;

    /// Master-transmit: the master wrote `byte` to this device. Returns
    /// whether the device acknowledges (true = ACK).
    fn write(&mut self, byte: u8) -> bool;

    /// Master-receive: the master is reading a byte from this device.
    /// `master_will_ack` is whether the *master* will ACK this byte (true)
    /// or NACK it (false, signalling "this is the last byte I want").
    fn read(&mut self, master_will_ack: bool) -> u8;

    /// STOP condition (or the device being deselected by a repeated
    /// START to a different address).
    fn stop(&mut self);
}

/// What [`Pcf8584`](crate::pcf8584::Pcf8584) drives. A thin trait boundary
/// so the controller's state machine doesn't need to know about the
/// device registry at all -- see [`I2cBusEngine`] for the real
/// implementation and the module docs for why this split exists.
pub trait I2cBus {
    fn start(&mut self, addr7: u8, read: bool) -> bool;
    fn write(&mut self, byte: u8) -> bool;
    fn read(&mut self, master_will_ack: bool) -> u8;
    fn stop(&mut self);
}

/// The real bus: a flat list of (address, device) pairs. Addresses are
/// not required to be unique -- a real bus with two conflicting devices
/// is a valid (if unusual) fixture, and it's not this engine's job to
/// second-guess the scenario/config that built it. On a collision the
/// first-attached device wins the address phase.
pub struct I2cBusEngine {
    devices: Vec<(u8, Box<dyn I2cDevice>)>,
    active: Option<usize>,
}

impl I2cBusEngine {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            active: None,
        }
    }

    pub fn attach(&mut self, addr7: u8, device: Box<dyn I2cDevice>) {
        self.devices.push((addr7, device));
    }
}

impl Default for I2cBusEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cBus for I2cBusEngine {
    fn start(&mut self, addr7: u8, read: bool) -> bool {
        self.active = self.devices.iter().position(|(a, _)| *a == addr7);
        match self.active {
            Some(i) => self.devices[i].1.start(read),
            None => false, // no device at this address: NAK, like an empty bus
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        match self.active {
            Some(i) => self.devices[i].1.write(byte),
            None => false,
        }
    }

    fn read(&mut self, master_will_ack: bool) -> u8 {
        match self.active {
            Some(i) => self.devices[i].1.read(master_will_ack),
            None => 0xFF, // an unaddressed/open bus reads as all-ones
        }
    }

    fn stop(&mut self) {
        if let Some(i) = self.active.take() {
            self.devices[i].1.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Recorder {
        acked_start: bool,
        acked_write: bool,
        last_write: Option<u8>,
        read_value: u8,
        stopped: bool,
    }

    impl I2cDevice for Recorder {
        fn start(&mut self, _read: bool) -> bool {
            self.acked_start
        }
        fn write(&mut self, byte: u8) -> bool {
            self.last_write = Some(byte);
            self.acked_write
        }
        fn read(&mut self, _master_will_ack: bool) -> u8 {
            self.read_value
        }
        fn stop(&mut self) {
            self.stopped = true;
        }
    }

    #[test]
    fn unaddressed_bus_naks_and_reads_open_bus() {
        let mut bus = I2cBusEngine::new();
        assert!(!bus.start(0x20, false));
        assert_eq!(bus.read(true), 0xFF);
    }

    #[test]
    fn start_selects_device_by_address_and_routes_subsequent_calls() {
        let mut bus = I2cBusEngine::new();
        bus.attach(
            0x20,
            Box::new(Recorder {
                acked_start: true,
                acked_write: true,
                last_write: None,
                read_value: 0xAB,
                stopped: false,
            }),
        );
        // A different address on the same bus should not be selected.
        bus.attach(
            0x50,
            Box::new(Recorder {
                acked_start: true,
                acked_write: true,
                last_write: None,
                read_value: 0x00,
                stopped: false,
            }),
        );

        assert!(bus.start(0x20, false));
        assert!(bus.write(0x42));
        assert_eq!(bus.read(true), 0xAB);
        bus.stop();
    }

    #[test]
    fn stop_deselects_the_active_device() {
        let mut bus = I2cBusEngine::new();
        bus.attach(
            0x20,
            Box::new(Recorder {
                acked_start: true,
                acked_write: true,
                last_write: None,
                read_value: 0,
                stopped: false,
            }),
        );
        bus.start(0x20, false);
        bus.stop();
        // With no active device, a further write should just NAK.
        assert!(!bus.write(0x01));
    }
}
