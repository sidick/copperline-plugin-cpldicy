# copperline-plugin-cpldicy

A [Copperline](https://github.com/CopperlineHQ/Copperline) WASM Zorro
plugin emulating Henryk Richter's [CPLDIcy](https://gitlab.com/HenrykRichter/cpldicy)
I2C card — a PCF8584-based Zorro II board, software-compatible with M.
Boehmer's original ICY board — plus its authentic LTC2990 voltage/
temperature monitor and Fanny-compatible MAX31760 fan controller, and a
scriptable virtual I2C bus of teaching-sample peripherals (GPIO
expander, EEPROM, LM75 sensor, four `SetClockI2C`-supported RTCs —
PCF8583/DS1307/DS1629/R2025 — and an HD44780 character LCD).

Also intended as a worked reference for Copperline's WASM Zorro plugin
mechanism generally — see `docs/tutorial.md` for a from-scratch,
self-contained tutorial with its own minimal worked example, if that's
what brought you here rather than the I2C card itself.

See `docs/board-facts.md` for the primary-source register-level facts
this emulation is built from. Open follow-up work is tracked as
[GitHub issues](https://github.com/sidick/copperline-plugin-cpldicy/issues)
rather than a standing plan doc, now that the initial implementation is
complete.

## Installation

There is no install step in the traditional sense — nothing gets copied
into Copperline's own directories. A plugin is just a manifest file plus
a `.wasm` file next to it; Copperline reads them from wherever you keep
them, and only cares about the path to the manifest.

### Step 1: get the files

**Option A — download, no toolchain needed.** Grab the latest release's
`.zip` from the [Releases page](https://github.com/sidick/copperline-plugin-cpldicy/releases)
and unzip it somewhere you'll keep it (don't move the files around
inside — `manifest/cpldicy.toml`'s reference to the `.wasm` is a
relative path, so the two must stay in the same relative positions
you unzipped them in). Each tagged release is built and published
automatically (`.github/workflows/release.yml`), gated on the full test
suite passing, so any release you download has already passed CI.

**Option B — build from source.**

```sh
git clone https://github.com/sidick/copperline-plugin-cpldicy
cd copperline-plugin-cpldicy
rustup target add wasm32-unknown-unknown
make          # builds target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm
make test     # optional: native unit + integration tests
```

Either way, you end up with the same two things: a `manifest/cpldicy.toml`
and, alongside it (one directory up, at
`target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm`), the
compiled plugin the manifest points at.

### Step 2: point Copperline at the manifest

Pick whichever of these two is more convenient — they produce/read the
identical configuration, so you can also switch between them later.

**Via the GUI:**

1. Launch Copperline with no `--config` (or with one you want to add the
   board to) — it opens on the machine configuration screen.
2. Click the **Zorro** tab in the category sidebar.
3. Click **Add board...** — a native file picker opens ("Add Zorro board
   metadata"). Browse to the `manifest/cpldicy.toml` from step 1 and
   select it.
4. The board appears with a header row (its declared name, "CPLDIcy
   I2C") and one row per config option below it — toggle buttons for the
   `pcf8574`/`eeprom`/`lm75`/`ltc2990`/`pcf8583`/`ds1307`/`ds1629`/`r2025`/
   `lcd`/`fan` bools, a stepper for `eeprom_size`/`lcd_columns`, and
   **Browse**/**Clear** buttons for the `file`-typed `eeprom_image`/
   `scenario` options (see the table below for what each does).
5. Click **Run** to boot with the board fitted, or use the **Save As**/
   **Save default** actions at the bottom of the screen to persist this
   configuration to a `.toml` file (or as Copperline's own default) for
   next time — the GUI writes exactly the `[[zorro]]` TOML shown below,
   so a saved config remains hand-editable afterward.

**Via a config file:** add a `[[zorro]]` entry to a Copperline machine
config pointing at the manifest, then launch with `copperline --config
that-file.toml`:

```toml
[[zorro]]
metadata = "path/to/cpldicy-plugin/manifest/cpldicy.toml"
config = { ltc2990 = "true", fan = "true", scenario = "my-scenario.txt" }
```

That's the whole install: no files land anywhere Copperline itself
manages, and removing the board later is just deleting the `[[zorro]]`
entry (or clicking **Remove** in the GUI) — nothing to uninstall.

### Config options

| Key | Default | Meaning |
|---|---|---|
| `pcf8574` | `true` | GPIO expander sample device (address 0x20) |
| `eeprom` | `false` | 24Cxx EEPROM sample device (address 0x54) |
| `eeprom_size` | `4096` | EEPROM size in bytes |
| `eeprom_image` | — | `type=file`: initial EEPROM contents |
| `lm75` | `false` | LM75 temperature sensor sample device (address 0x48) |
| `ltc2990` | `true` | the real board's own authentic monitor chip (address 0x4C) |
| `pcf8583` | `false` | RTC sample device (address 0x50) |
| `pcf8583_time` | — | `"YYYY-MM-DD HH:MM:SS"`: initial time (defaults to the epoch if unset) |
| `ds1307` | `false` | RTC sample device (fixed address 0x68) |
| `ds1307_time` | — | `"YYYY-MM-DD HH:MM:SS"`: initial time (defaults to the epoch if unset) |
| `ds1629` | `false` | RTC sample device (fixed address 0x4F, command-dispatch protocol) |
| `ds1629_time` | — | `"YYYY-MM-DD HH:MM:SS"`: initial time (defaults to the epoch if unset) |
| `r2025` | `false` | RTC sample device (fixed address 0x32) |
| `r2025_time` | — | `"YYYY-MM-DD HH:MM:SS"`: initial time (defaults to the epoch if unset) |
| `lcd` | `false` | HD44780 character LCD sample device, PCF8574 I2C backpack (address 0x27) |
| `lcd_columns` | `16` | visible characters per row (16x2 is the common physical size) |
| `fan` | `true` | the real board's own authentic MAX31760 fan controller (address 0x50) |
| `scenario` | — | `type=file`: deterministic event timeline, see `plugin/src/scenario.rs` |

The four `_time` options don't default to the wall-clock time of the
machine running Copperline -- the plugin ABI has no host-time import to
read it from (see [issue #10](https://github.com/sidick/copperline-plugin-cpldicy/issues/10)).
Leave a `_time` option unset and the corresponding RTC starts at a fixed
epoch instead.

The LCD's visible text is exported by logging it (`wasm[cpldicy]: lcd:
"row0" / "row1"` in Copperline's own log) whenever it changes -- the
plugin ABI has no display output of its own to render it into, so this
is as far as a *character* display's output can travel today. A
graphical I2C display (an SSD1306 OLED, say) would need an actual host
rendering capability to be worth adding at all; see
[issue #11](https://github.com/sidick/copperline-plugin-cpldicy/issues/11).

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
| `FannyCtl` (Henryk Richter, [`i2csensors`](https://gitlab.com/HenrykRichter/i2csensors/-/tree/master) repo) | Reads the MAX31760's full register/LUT state at its documented default address (0xA0) without error |
| `i2csensors.library` + `simplesensors` | Opens, reads LTC2990 voltage/temperature channels matching the configured values (VCC 5.0000V, V1 5.0001V, V2 11.9999V, Tint 25.0000°C), reads the MAX31760's fan/temperature channels, and (via `examples/Sensors/LM75.cfg`, this project's own since no official one exists upstream) reads the LM75's temperature channel matching its configured value (25.0000°C default) |
| `diagnostics` | Confirms `i2c.library`/`i2csensors.library` present, all three `Devs:Sensors/*.cfg` config files parsed successfully |
| `I2Clock` (Henryk Richter, `i2csensors` repo) | `SCAN` identifies all four RTCs by vendor/chip name at their real addresses (`0x9E`/DS1629, `0xA0`/PCF8583, `0xD0`/DS1307, `0x64`/R2025); `SAVE` then `SHOW` per chip proves the bus-write path, not just reads — it stores the guest's live system time on the chip and reads it straight back |

This is the full oracle validation `PLAN.md`'s Phase 1/2 success criteria
call for: unmodified `i2c.library` plus at least two existing Aminet
tools work against the board with no special casing, across all three of
the board's functions (I2C GPIO/bus, LTC2990 monitoring, fan control).

Reproduce with `make oracle` (needs `xdftool`, a Copperline build, and
either `m68k-amigaos-gcc` on `PATH` or Docker for the tier-2 probe build
— see `tests/copperline/run.sh`/`run-oracle.sh` headers for exact
prereqs). The fetched binaries land in `nondistributable/` (git-ignored,
never committed — see `vendor/fetch-oracle.sh` for provenance/licensing
notes on each). All of `FannyCtl`/`i2csensors.library`/`simplesensors`/
`diagnostics` above come from Henryk Richter's
[`i2csensors`](https://gitlab.com/HenrykRichter/i2csensors/-/tree/master)
repo — useful general-purpose Amiga I2C tooling beyond just this
board's own oracle pass.

Not every device has (or can have) a `Devs:Sensors/*.cfg` sample: that
format only covers `i2csensors.library`'s five sensor types (`TEMP`,
`VOLTAGE`, `CURRENT`, `FAN`, `PRESSURE`) — LTC2990 and MAX31760 already
had official ones upstream, LM75 got its own above, but the RTCs don't
fit that format at all (there's no clock/calendar sensor type; `I2Clock`
is the right tool for those instead), and neither do PCF8574 (GPIO),
the EEPROM, or the LCD. If you write your own `Devs:Sensors/*.cfg`, two
real quirks of `i2csensors.library`'s parser (`sensors/src/config.c`)
are worth knowing: it expects Latin-1, not UTF-8 (a `°` written as UTF-8
silently breaks a config file's device count), and it treats a closing
`]` as ending the current section *even inside a `#` comment* — both
found the hard way while writing `examples/Sensors/LM75.cfg`.

## License

BSD 2-Clause — see `LICENSE`. Chosen deliberately for the "reference
plugin" goal: permissive enough that anyone can copy this repo's
patterns (or the tutorial's `TimerPort` example) into their own plugin,
under any license, without being pulled into copyleft terms. This is
independent of, and doesn't resolve, the separate question of whether a
Copperline plugin is itself a derivative work of GPLv3 Copperline —
Copperline's own `LICENSE` and `docs/` carry no explicit plugin/linking
exception either way.
