# copperline-plugin-cpldicy

A [Copperline](https://github.com/CopperlineHQ/Copperline) WASM Zorro
plugin emulating Henryk Richter's [CPLDIcy](https://gitlab.com/HenrykRichter/cpldicy)
I2C card — a PCF8584-based Zorro II board, software-compatible with M.
Boehmer's original ICY board — plus its authentic LTC2990 voltage/
temperature monitor and Fanny-compatible MAX31760 fan controller, and a
scriptable virtual I2C bus of teaching-sample peripherals (GPIO
expander, EEPROM, LM75 sensor, PCF8583 clock).

Also intended as a worked reference for Copperline's WASM Zorro plugin
mechanism generally — see `docs/tutorial.md` for a from-scratch,
self-contained tutorial with its own minimal worked example, if that's
what brought you here rather than the I2C card itself.

See `PLAN.md` for the implementation plan and phase breakdown, and
`docs/board-facts.md` for the primary-source register-level facts this
emulation is built from.

## Building

```sh
rustup target add wasm32-unknown-unknown
make          # builds target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm
make test     # native unit + integration tests
```

## Using it

Add a `[[zorro]]` entry to a Copperline machine config pointing at the
manifest:

```toml
[[zorro]]
metadata = "path/to/copperline-plugin-cpldicy/manifest/cpldicy.toml"
config = { ltc2990 = "true", fan = "true", scenario = "my-scenario.txt" }
```

### Config options

| Key | Default | Meaning |
|---|---|---|
| `pcf8574` | `true` | GPIO expander sample device (address 0x20) |
| `eeprom` | `false` | 24Cxx EEPROM sample device (address 0x54) |
| `eeprom_size` | `4096` | EEPROM size in bytes |
| `eeprom_image` | — | `type=file`: initial EEPROM contents |
| `lm75` | `false` | LM75 temperature sensor sample device (address 0x48) |
| `ltc2990` | `true` | the real board's own authentic monitor chip (address 0x4C) |
| `pcf8583` | `false` | RTC sample device (address 0x51) |
| `fan` | `true` | the real board's own authentic MAX31760 fan controller (address 0x50) |
| `scenario` | — | `type=file`: deterministic event timeline, see `plugin/src/scenario.rs` |

### Scenario format

A small line-based format (deliberately not TOML — see
`plugin/src/scenario.rs`'s module docs for why): `<cck> <verb> <args>`
per line, blank lines and `#`-comments ignored.

```
0 set ltc2990.tint 25.0
1000000 set ltc2990.tint 60.0
2000000 fault fan stuck_rotor
```

Verbs: `set <device>.<field> <value>` (targets: `ltc2990.tint`,
`ltc2990.v1`, `ltc2990.v2`, `ltc2990.external_temp`, `ltc2990.vcc`,
`lm75.celsius`), and `fault <device> <kind>` (`unplugged`/`ok` for any
device — an address-NAK fixture; `stuck_rotor`/`spinning` for the fan).

## Testing

Three tiers, per `PLAN.md` section 4:

1. **Native unit + integration tests** — `make test`. Covers the
   PCF8584 state machine, every device model, the fan controller, the
   scenario engine, and flagship deterministic scenarios (a closed
   thermal loop, a fault fixture, byte-identical replay).
2. **Headless conformance probe** — `make conformance`. A freestanding
   m68k probe pokes the board window directly under real Copperline
   AutoConfig/bus timing: AutoConfig discovery, register reset defaults,
   register round-trip, and a full I2C master-tx/master-rx transaction.
3. **Guest oracle pass** — `make oracle`. Fetches (`make fetch-oracle`)
   and boots *unmodified* third-party AmigaOS software against the
   board — no code in this repo touches the guest side. See below.

## Oracle compatibility

Validated 2026-08-16 against real, unmodified AmigaOS software — nothing
in this repository patches or special-cases any of it:

| Software | Result |
|---|---|
| `i2c.library` v40 ("bcu"/PCF8584 driver, Wilhelm Noeker/Brian Ipsen) | Detects the board via AutoConfig (manufacturer 5001, product 15) |
| `I2CScan` (Aminet `docs/hard/i2clib40`) | Full bus scan finds all three default devices, each confirmed via a real address+W *and* address+R (1-byte master-receive, exercising the dummy-read pipeline) transaction: `0x40/0x41` (PCF8574), `0x98/0x99` (LTC2990), `0xa0/0xa1` (MAX31760) |
| `FannyCtl` (Henryk Richter, `i2csensors` repo) | Reads the MAX31760's full register/LUT state at its documented default address (0xA0) without error |
| `i2csensors.library` + `simplesensors` | Opens, reads LTC2990 voltage/temperature channels matching the configured values (VCC 5.0000V, V1 5.0001V, V2 11.9999V, Tint 25.0000°C), reads the MAX31760's fan/temperature channels |
| `diagnostics` | Confirms `i2c.library`/`i2csensors.library` present, both `Devs:Sensors/*.cfg` config files parsed successfully |

This is the full oracle validation `PLAN.md`'s Phase 1/2 success criteria
call for: unmodified `i2c.library` plus at least two existing Aminet
tools work against the board with no special casing, across all three of
the board's functions (I2C GPIO/bus, LTC2990 monitoring, fan control).

Reproduce with `make oracle` (needs `xdftool`, a Copperline build, and
either `m68k-amigaos-gcc` on `PATH` or Docker for the tier-2 probe build
— see `tests/copperline/run.sh`/`run-oracle.sh` headers for exact
prereqs). The fetched binaries land in `nondistributable/` (git-ignored,
never committed — see `vendor/fetch-oracle.sh` for provenance/licensing
notes on each).
