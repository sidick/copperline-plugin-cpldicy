//! The two Phase 2 flagship tests plus a determinism check (docs/PLAN.md
//! sections 3.6 and 5, success criteria): a closed thermal loop (scripted
//! LTC2990 temperature -> "guest" fan-curve response -> asserted PWM/RPM
//! rise), a fault fixture (an unplugged sensor NAK's visibly instead of
//! hanging the bus), and byte-identical replay of a scripted run.
//!
//! These drive [`Board`] through its *real* I2C register protocol (the
//! same `read`/`write`/`tick` sequence `i2c.library` would issue), not
//! through the scenario-setter shortcuts `board.rs`'s own unit tests use
//! -- standing in for the guest-side fan-curve software and sensor
//! driver docs/PLAN.md describes, since the actual FannyCtl/simplesensors
//! Aminet binaries aren't fetched into `nondistributable/` yet (see
//! docs/board-facts.md §7's open item). Wiring a real m68k guest probe
//! against these same scripted scenarios is a natural tier-2 conformance
//! rig extension once those binaries land, but isn't required for these
//! host-side, deterministic tests to do their job now.

use cpldicy_plugin::board::Board;
use cpldicy_plugin::fan::MAX31760_ADDRESS;
use cpldicy_plugin::pcf8584::CCK_PER_BYTE_PHASE;
use cpldicy_plugin::scenario::Scenario;

const LTC2990_ADDRESS: u8 = 0x4C;
const REG_TINT_MSB: u8 = 0x04;
const REG_PWMV: u8 = 0x51;

const PIN: u32 = 0x80;
const ESO: u32 = 0x40;
const STA: u32 = 0x04;
const STO: u32 = 0x02;
const ACK: u32 = 0x01;

fn select_s0(board: &mut Board) {
    board.write(2, 1, PIN | ESO | ACK);
}

/// A full master-transmit transaction: address + all of `bytes`, then
/// STOP. Returns `false` (and still issues STOP, leaving the bus clean)
/// if the address phase itself was NAK'd -- the "sensor unplugged" fault
/// fixture's observable signal.
fn master_write(board: &mut Board, addr7: u8, bytes: &[u8]) -> bool {
    select_s0(board);
    board.write(0, 1, u32::from(addr7) << 1);
    board.write(2, 1, PIN | ESO | STA | ACK);
    board.tick(CCK_PER_BYTE_PHASE);
    if board.read(2, 1) & 0x08 != 0 {
        board.write(2, 1, PIN | ESO | STO | ACK);
        return false;
    }
    for &b in bytes {
        board.write(0, 1, u32::from(b));
        board.tick(CCK_PER_BYTE_PHASE);
    }
    board.write(2, 1, PIN | ESO | STO | ACK);
    true
}

/// A full master-receive transaction of `n` bytes, implementing the
/// PCF8584 dummy-read pipeline exactly as docs/board-facts.md section 4
/// describes it: `N+1` total S0 reads for `N` data bytes, with ACK
/// cleared immediately before the read call that arms the *final* byte
/// (call index `n-1`, where call index 0 is the dummy read) so the
/// device sees a NACK on its last byte. Returns `None` if the address
/// phase was NAK'd.
fn master_read_bytes(board: &mut Board, addr7: u8, n: usize) -> Option<Vec<u8>> {
    select_s0(board);
    board.write(0, 1, (u32::from(addr7) << 1) | 1);
    board.write(2, 1, PIN | ESO | STA | ACK);
    board.tick(CCK_PER_BYTE_PHASE);
    if board.read(2, 1) & 0x08 != 0 {
        board.write(2, 1, PIN | ESO | STO | ACK);
        return None;
    }
    if n == 0 {
        board.write(2, 1, PIN | ESO | STO | ACK);
        return Some(Vec::new());
    }

    let mut bytes = Vec::with_capacity(n);
    for call_index in 0..=n {
        if call_index == n - 1 {
            board.write(2, 1, PIN | ESO); // ACK=0: this call arms the final byte
        }
        let byte = board.read(0, 1) as u8;
        if call_index >= 1 {
            bytes.push(byte); // call 0 is the dummy read
        }
        if call_index == n {
            break; // final buffer-only fetch, already past STOP
        }
        if call_index == n - 1 {
            board.tick(CCK_PER_BYTE_PHASE); // clock in the final byte
            board.write(2, 1, PIN | ESO | STO | ACK); // STOP right after
        } else {
            board.tick(CCK_PER_BYTE_PHASE);
        }
    }
    Some(bytes)
}

/// Mirrors `devices::ltc2990::encode_temp13`'s format: 13-bit two's
/// complement, 0.0625C/LSB, DATA_VALID in the MSB's top bit.
fn decode_temp13(msb: u8, lsb: u8) -> f32 {
    let raw = ((i32::from(msb & 0x1F)) << 8) | i32::from(lsb);
    let raw = if raw & 0x1000 != 0 { raw - 0x2000 } else { raw };
    raw as f32 / 16.0
}

fn read_ltc2990_tint(board: &mut Board) -> f32 {
    assert!(master_write(board, LTC2990_ADDRESS, &[REG_TINT_MSB]));
    let bytes = master_read_bytes(board, LTC2990_ADDRESS, 2).expect("LTC2990 should ACK");
    decode_temp13(bytes[0], bytes[1])
}

fn write_fan_duty(board: &mut Board, duty: u8) {
    assert!(master_write(board, MAX31760_ADDRESS, &[REG_PWMV, duty]));
}

/// A deliberately simple fan curve: off below 40C, full speed above
/// 70C, linear ramp between -- standing in for the "FannyCtl-configured
/// curve" docs/PLAN.md describes; this test acts as its own guest, so it
/// needs *some* curve, not FannyCtl's actual default one.
fn fan_curve(celsius: f32) -> u8 {
    if celsius < 40.0 {
        0
    } else if celsius > 70.0 {
        255
    } else {
        (((celsius - 40.0) / 30.0) * 255.0) as u8
    }
}

#[test]
fn closed_thermal_loop_scripted_temperature_drives_fan_response() {
    let mut board = Board::new(); // LTC2990 + fan enabled by default
    let mut scenario = Scenario::parse(
        "0 set ltc2990.tint 25.0\n\
         1000 set ltc2990.tint 60.0\n",
    )
    .unwrap();

    // Before the ramp: cool, fan should be commanded off.
    scenario.tick(1, &mut board);
    let cool_temp = read_ltc2990_tint(&mut board);
    assert!((cool_temp - 25.0).abs() < 0.1);
    write_fan_duty(&mut board, fan_curve(cool_temp));
    assert_eq!(board.fan_duty(), Some(0));
    assert_eq!(board.fan_rpm(), Some(0));

    // Advance the scenario past the temperature ramp.
    scenario.tick(2000, &mut board);
    let hot_temp = read_ltc2990_tint(&mut board);
    assert!((hot_temp - 60.0).abs() < 0.1, "scripted temperature should have risen to 60C, got {hot_temp}");

    let target_duty = fan_curve(hot_temp);
    assert!(target_duty > 0, "60C should be inside the fan curve's active range");
    write_fan_duty(&mut board, target_duty);
    assert_eq!(board.fan_duty(), Some(target_duty));

    // Let the virtual fan physically spin up in response.
    board.tick(50_000);
    let rpm = board.fan_rpm().expect("fan should be enabled");
    assert!(rpm > 0, "the fan should have spun up in response to the scripted temperature rise, got {rpm} RPM");
}

#[test]
fn fault_fixture_sensor_nak_handled_visibly_not_hung() {
    let mut board = Board::new();
    let mut scenario = Scenario::parse("0 fault ltc2990 unplugged\n").unwrap();
    scenario.tick(1, &mut board);

    // A "guest" trying to talk to the unplugged sensor should see the
    // address phase NAK -- master_write returns false, not a hang.
    let reached_sensor = master_write(&mut board, LTC2990_ADDRESS, &[REG_TINT_MSB]);
    assert!(!reached_sensor, "an unplugged sensor should NAK visibly, not silently succeed");

    // The bus itself must recover: a different, still-plugged-in device
    // should work fine right after.
    let reached_fan = master_write(&mut board, MAX31760_ADDRESS, &[REG_PWMV, 0x10]);
    assert!(reached_fan, "the bus should recover for other devices after a NAK'd transaction");
}

#[test]
fn a_faulted_sensor_can_be_replugged_and_resume_working() {
    let mut board = Board::new();
    let mut scenario = Scenario::parse(
        "0 fault ltc2990 unplugged\n\
         1000 fault ltc2990 ok\n",
    )
    .unwrap();

    scenario.tick(1, &mut board);
    assert!(!master_write(&mut board, LTC2990_ADDRESS, &[REG_TINT_MSB]));

    scenario.tick(2000, &mut board);
    assert!(master_write(&mut board, LTC2990_ADDRESS, &[REG_TINT_MSB]), "should ACK again once replugged");
}

#[test]
fn scripted_run_replays_byte_identically() {
    fn run() -> Vec<u8> {
        let mut board = Board::new();
        let mut scenario = Scenario::parse(
            "0 set ltc2990.tint 25.0\n\
             1000 set ltc2990.tint 55.0\n\
             2500 fault fan stuck_rotor\n",
        )
        .unwrap();

        let mut trace = Vec::new();
        for _ in 0..6 {
            scenario.tick(500, &mut board);
            board.tick(500);
            let bytes = master_read_bytes(&mut board, LTC2990_ADDRESS, 2).unwrap();
            trace.extend_from_slice(&bytes);
            trace.push(board.fan_rpm().unwrap_or(0).min(0xFF) as u8);
        }
        trace
    }

    let first = run();
    let second = run();
    assert_eq!(first, second, "the same scripted timeline should replay byte-identically");
}
