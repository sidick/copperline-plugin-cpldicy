//! Copperline WASM Zorro board: CPLDIcy I2C card.
//!
//! An emulation of Henryk Richter's CPLDIcy Zorro II I2C card
//! (PCF8584-based, software-compatible with M. Boehmer's original ICY
//! board) as a Copperline plugin, per docs/PLAN.md. This file is the only
//! one that touches the Copperline plugin ABI (docs/zorro.md); everything
//! else is pure Rust the native `cargo test` target exercises directly.
//!
//! - [`pcf8584`] -- the chip model: registers, control/status bits, the
//!   START/STOP/data transaction state machine.
//! - [`i2c`] -- the virtual bus: [`i2c::I2cDevice`] and the registry
//!   [`pcf8584::Pcf8584`] drives transactions through.
//! - [`devices`] -- bus residents (just the PCF8574 GPIO expander in
//!   Phase 1).
//! - [`board`] -- CPLDIcy's own address decode (byte lanes, register
//!   mirroring) wiring the chip model to the bus.
//!
//! All mutable state lives inside [`BOARD`]'s `RefCell`, reachable from
//! linear memory -- Copperline's save states only snapshot linear memory,
//! never WASM globals (docs/zorro.md), so nothing here may use `static
//! mut` or hold state any other way.

pub mod board;
pub mod devices;
pub mod i2c;
pub mod pcf8584;

use board::Board;
use core::cell::RefCell;

// ---------------------------------------------------------------------------
// Host imports (Copperline plugin ABI, module "env"). Signatures follow
// docs/zorro.md. Only `log` is used in Phase 1 -- config_get/resource_*
// land in Phase 2 with the scenario/EEPROM-image config surface.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn log(ptr: i32, len: i32);
}

// Native stub so `cargo test` links and unit tests can drive the board
// directly without a WASM host present.
#[cfg(not(target_arch = "wasm32"))]
mod host_stubs {
    #[allow(unused_variables)]
    pub unsafe fn log(ptr: i32, len: i32) {}
}
#[cfg(not(target_arch = "wasm32"))]
use host_stubs::*;

/// Log a line through the host (`wasm[cpldicy]: ...` in Copperline's own
/// log, per docs/zorro.md).
#[allow(dead_code)]
pub fn host_log(msg: &str) {
    unsafe { log(msg.as_ptr() as i32, msg.len() as i32) }
}

thread_local! {
    static BOARD: RefCell<Board> = RefCell::new(Board::new());
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn init() {
    host_log("cpldicy: init");
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn read(off: i32, size: i32) -> i32 {
    BOARD.with(|b| b.borrow_mut().read(off as u32, size as u32) as i32)
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn write(off: i32, size: i32, value: i32) {
    BOARD.with(|b| b.borrow_mut().write(off as u32, size as u32, value as u32));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn tick(cck: i32) {
    BOARD.with(|b| b.borrow_mut().tick(cck as u32));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn int2() -> i32 {
    BOARD.with(|b| i32::from(b.borrow().int2_asserted()))
}
