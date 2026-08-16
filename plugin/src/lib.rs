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
//! - [`devices`] -- bus residents (PCF8574, EEPROM24, LM75, LTC2990,
//!   PCF8583).
//! - [`fan`] -- the MAX31760 fan controller and its virtual fan.
//! - [`board`] -- CPLDIcy's own address decode (byte lanes, register
//!   mirroring) wiring the chip model to the bus and the optional
//!   devices together.
//! - [`scenario`] -- the deterministic cck-keyed event timeline
//!   (docs/PLAN.md section 3.6), driven from the `scenario` config
//!   resource.
//!
//! All mutable state lives inside [`STATE`]'s `RefCell`, reachable from
//! linear memory -- Copperline's save states only snapshot linear memory,
//! never WASM globals (docs/zorro.md), so nothing here may use `static
//! mut` or hold state any other way.

// The host-config wiring below (config_get/resource_* imports,
// board_config_from_host, scenario_from_host, PluginState) is only ever
// reached from the wasm32-gated ABI exports -- native tests build a
// Board/Scenario directly instead (see host_stubs's own doc comment), so
// this code is genuinely unreachable under `cargo test` too, not just a
// plain native build. Unlike copperline-bridgeboard-plugin's `Board`
// (which its own tests drive directly), nothing here overlaps with what
// the tests exercise.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

pub mod board;
pub mod devices;
pub mod fan;
pub mod i2c;
pub mod pcf8584;
pub mod rtc_time;
pub mod scenario;

use board::{Board, BoardConfig};
use core::cell::RefCell;
use scenario::Scenario;

// ---------------------------------------------------------------------------
// Host imports (Copperline plugin ABI, module "env"). Signatures follow
// docs/zorro.md.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn log(ptr: i32, len: i32);
    fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn resource_len(key_ptr: i32, key_len: i32) -> i32;
    fn resource_read(key_ptr: i32, key_len: i32, off: i32, out_ptr: i32, out_cap: i32) -> i32;
}

// Native stubs so `cargo test` links and unit tests can drive the board
// directly without a WASM host present. All config/resource lookups
// report "absent" -- native tests build a `Board`/`Scenario` directly
// instead of going through `init()`'s host-config path.
#[cfg(not(target_arch = "wasm32"))]
mod host_stubs {
    #[allow(unused_variables)]
    pub unsafe fn log(ptr: i32, len: i32) {}
    #[allow(unused_variables)]
    pub unsafe fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32 {
        -1
    }
    #[allow(unused_variables)]
    pub unsafe fn resource_len(key_ptr: i32, key_len: i32) -> i32 {
        -1
    }
    #[allow(unused_variables)]
    pub unsafe fn resource_read(
        key_ptr: i32,
        key_len: i32,
        off: i32,
        out_ptr: i32,
        out_cap: i32,
    ) -> i32 {
        -1
    }
}
#[cfg(not(target_arch = "wasm32"))]
use host_stubs::*;

/// Log a line through the host (`wasm[cpldicy]: ...` in Copperline's own
/// log, per docs/zorro.md).
pub fn host_log(msg: &str) {
    unsafe { log(msg.as_ptr() as i32, msg.len() as i32) }
}

/// Read a string setting (manifest `[config]` defaults layered under the
/// user's per-board overrides). `None` if absent.
fn config_get_string(key: &str) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe {
        config_get(
            key.as_ptr() as i32,
            key.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if n < 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..(n as usize).min(buf.len())]).into_owned())
}

fn config_get_bool(key: &str, default: bool) -> bool {
    match config_get_string(key).as_deref() {
        Some("true") | Some("1") => true,
        Some("false") | Some("0") => false,
        _ => default,
    }
}

fn config_get_usize(key: &str, default: usize) -> usize {
    config_get_string(key)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Reads an I2C address config value -- `"0x9A"`/`"0X9A"` (matching how
/// this project's own doc comments and Henryk Richter's `i2csensors`
/// config files write addresses) or plain decimal, either accepted.
fn config_get_u8_address(key: &str, default: u8) -> u8 {
    config_get_string(key)
        .and_then(|s| {
            let s = s.trim();
            match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Some(hex) => u8::from_str_radix(hex, 16).ok(),
                None => s.parse().ok(),
            }
        })
        .unwrap_or(default)
}

/// Reads a `<device>_time` config string, if present, and parses it via
/// [`rtc_time::parse`]. A malformed value is logged and treated as
/// absent (the device keeps its epoch default) rather than failing the
/// whole board -- same "don't let one bad config value take the rest of
/// the board down with it" choice `scenario_from_host` makes for a
/// malformed scenario file.
fn rtc_time_from_host(key: &str) -> Option<rtc_time::WallClock> {
    let raw = config_get_string(key)?;
    match rtc_time::parse(&raw) {
        Ok(time) => Some(time),
        Err(e) => {
            host_log(&format!("cpldicy: {key} config error: {e}"));
            None
        }
    }
}

/// Read an entire file-typed resource (e.g. `eeprom_image`, `scenario`).
/// `None` if the resource is absent.
fn resource_get(key: &str) -> Option<Vec<u8>> {
    let len = unsafe { resource_len(key.as_ptr() as i32, key.len() as i32) };
    if len < 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let n = unsafe {
        resource_read(
            key.as_ptr() as i32,
            key.len() as i32,
            0,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

fn board_config_from_host() -> BoardConfig {
    let defaults = BoardConfig::default();
    BoardConfig {
        pcf8574_enabled: config_get_bool("pcf8574", defaults.pcf8574_enabled),
        eeprom_enabled: config_get_bool("eeprom", defaults.eeprom_enabled),
        eeprom_size: config_get_usize("eeprom_size", defaults.eeprom_size),
        eeprom_image: resource_get("eeprom_image"),
        lm75_enabled: config_get_bool("lm75", defaults.lm75_enabled),
        ltc2990_enabled: config_get_bool("ltc2990", defaults.ltc2990_enabled),
        ltc2990_address: config_get_u8_address("ltc2990_address", defaults.ltc2990_address),
        pcf8583_enabled: config_get_bool("pcf8583", defaults.pcf8583_enabled),
        pcf8583_address: config_get_u8_address("pcf8583_address", defaults.pcf8583_address),
        pcf8583_time: rtc_time_from_host("pcf8583_time"),
        ds1307_enabled: config_get_bool("ds1307", defaults.ds1307_enabled),
        ds1307_time: rtc_time_from_host("ds1307_time"),
        ds1629_enabled: config_get_bool("ds1629", defaults.ds1629_enabled),
        ds1629_time: rtc_time_from_host("ds1629_time"),
        r2025_enabled: config_get_bool("r2025", defaults.r2025_enabled),
        r2025_time: rtc_time_from_host("r2025_time"),
        lcd_enabled: config_get_bool("lcd", defaults.lcd_enabled),
        lcd_columns: config_get_usize("lcd_columns", defaults.lcd_columns),
        bmp280_enabled: config_get_bool("bmp280", defaults.bmp280_enabled),
        bme680_enabled: config_get_bool("bme680", defaults.bme680_enabled),
        am2320_enabled: config_get_bool("am2320", defaults.am2320_enabled),
        fan_enabled: config_get_bool("fan", defaults.fan_enabled),
        fan_address: config_get_u8_address("fan_address", defaults.fan_address),
        ..defaults
    }
}

fn scenario_from_host() -> Scenario {
    match resource_get("scenario") {
        None => Scenario::empty(),
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) => match Scenario::parse(&text) {
                Ok(scenario) => scenario,
                Err(e) => {
                    host_log(&format!("cpldicy: scenario parse error: {e}"));
                    Scenario::empty()
                }
            },
            Err(_) => {
                host_log("cpldicy: scenario resource is not valid UTF-8");
                Scenario::empty()
            }
        },
    }
}

struct PluginState {
    board: Board,
    scenario: Scenario,
}

impl PluginState {
    fn new() -> Self {
        Self {
            board: Board::with_config(board_config_from_host()),
            scenario: scenario_from_host(),
        }
    }
}

thread_local! {
    static STATE: RefCell<PluginState> = RefCell::new(PluginState::new());
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn init() {
    // Accessing STATE for the first time is what actually constructs it
    // (host config/resource calls happen inside `PluginState::new()`,
    // triggered by this first `.with()`) -- see docs/zorro.md's ABI
    // contract: `init` runs once after instantiation, before any
    // transaction, so this is the right (and only) place that first
    // access should happen.
    STATE.with(|_| {});
    host_log("cpldicy: init");
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn read(off: i32, size: i32) -> i32 {
    STATE.with(|s| s.borrow_mut().board.read(off as u32, size as u32) as i32)
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn write(off: i32, size: i32, value: i32) {
    STATE.with(|s| {
        s.borrow_mut()
            .board
            .write(off as u32, size as u32, value as u32)
    });
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn tick(cck: i32) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let PluginState { board, scenario } = &mut *s;
        board.tick(cck as u32);
        scenario.tick(cck as u32, board);
        // The LCD's only "export" path (no host framebuffer/display
        // import exists to render it anywhere else -- see the graphical
        // OLED follow-up issue this same gap forced). `lcd_text_if_changed`
        // already does its own diffing, so this only logs when the
        // visible content actually changed, not every tick.
        if let Some([row0, row1]) = board.lcd_text_if_changed() {
            host_log(&format!("cpldicy: lcd: {row0:?} / {row1:?}"));
        }
    });
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn int2() -> i32 {
    STATE.with(|s| i32::from(s.borrow().board.int2_asserted()))
}
