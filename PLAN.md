# Implementation Plan: CPLDIcy Copperline Plugin

Emulation of Henryk Richter's **CPLDIcy** Zorro II I²C card (PCF8584
programming model, ICY-compatible) as an out-of-tree **Copperline WASM
Zorro plugin**, with a Fanny-compatible PWM fan controller, an LTC2990
monitor, and a scriptable virtual I²C bus of standard peripherals.
Source proposal: `~/src/project-ideas/copperline-i2c-proposal.md`.

This plan is grounded in the actual Copperline plugin mechanism (verified
2026-08-15 against the sources below). Anything marked **[P0]** is a fact
still to be confirmed during Phase 0 reading — do not guess it during
implementation; read the referenced source first.

---

## 1. Reference material

**Copperline core** (`~/src/external/Copperline`):
- `docs/zorro.md` — THE plugin document: manifest format, module ABI,
  host imports, fault model, config schema, autoconfig, manufacturer IDs.
- `src/wasmboard.rs` — wasmtime host; ABI contract in the module doc
  comment (lines 1–120). Fuel budget, fault isolation, save-state rules.
- `src/zorro.rs` — manifest parsing (`load_board_metadata`), autoconfig.
- `src/zorro_device.rs` — `ZorroDevice` trait the host drives us through.
- `docs/debugger/control.md` — control protocol (CCP). **Note:**
  `mem.read`/`mem.write` are RAM-only; CCP cannot touch device windows.

**Template repo** (`~/src/copperline-bridgeboard-plugin`) — copy its shape:
- `plugin/src/lib.rs` — the one file touching the plugin ABI; wasm32-gated
  `#[no_mangle]` exports, native `host_stubs` for `cargo test`.
- `plugin/src/shim/{mod,layout,registers}.rs` — register-file board model:
  size-1/2/4 access decomposition, open-bus, per-register masks, reset
  defaults, level-sensitive INT2 with read-to-clear ack, unit tests.
- `manifest/a2088.toml` — compact I/O-board manifest.
- `tests/copperline/{run.sh,machine.toml,bridgeboard_probe.c,startup-sequence.txt}`
  — self-contained headless conformance rig (m68k-amigaos-gcc probe +
  xdftool boot floppy + `--serial stdout` + grep for `SUB=x=PASS`).
- Root `Makefile` (`all`/`test`/`clean`) and workspace `Cargo.toml`
  (`opt-level="z"`, `lto`, `panic="abort"`, `crate-type=["cdylib","rlib"]`).

**Secondary reference** (`~/src/copperline-hostsocket`): ABI edge-case
details; its `guest/` dir + `diag_vec` pattern only if we ever ship a
guest ROM (we do not plan to — `i2c.library` loads from disk).

**External [P0] reading (not on disk yet):**
- CPLDIcy sources: https://gitlab.com/HenrykRichter/cpldicy — register
  map, CPLD logic, AutoConfig identity, fan-controller registers, LTC2990
  wiring. CC-licensed: **read, never vendor**; the Rust is written fresh
  from the documented facts.
- PCF8584 datasheet (Philips/NXP) — the chip specification.
- LTC2990 datasheet — register map, conversion formats.
- PCF8574, 24Cxx, LM75, PCF8583 datasheets — virtual peripherals.
- Aminet: `i2c.library` (Wilhelm Noeker) + sources, I²C tools, FannyCtl,
  simplesensors/Sensei — the oracles. Fetch binaries into a
  `nondistributable/` dir (bridgeboard repo has the same convention).

**[P0] facts to pin down before coding** (record answers in
`docs/board-facts.md` with citations):
1. CPLDIcy AutoConfig manufacturer/product/serial and board size.
2. Register offsets of the PCF8584 within the board window (address
   decode, which address lines select S0–S3), and byte lanes (D0–D7 on
   which half of the 68000 data bus — affects odd/even addressing).
3. Interrupt line used (INT2 vs INT6) and how the interrupt is
   acknowledged/cleared on the real board.
4. Fan controller register interface (Fanny-compatible): offsets, duty
   register format, tach readback if any.
5. LTC2990 I²C address + configuration as wired on CPLDIcy/ICYv2, and
   which registers simplesensors/Sensei actually read.
6. Whether `i2c.library` autodetects via AutoConfig or needs a tooltype/
   config; which library version supports ICY.

---

## 2. Repo layout (target state)

```
copperline-plugin-cpldicy/
├── Cargo.toml                 # workspace: members = ["plugin"], release profile from template
├── Makefile                   # all / test / wasm / conformance / clean
├── README.md                  # user-facing: what it is, config snippet, oracle status
├── PLAN.md                    # this file
├── manifest/
│   └── cpldicy.toml           # AutoConfig identity + [config]/[[option]] schema
├── plugin/
│   ├── Cargo.toml             # crate-type = ["cdylib","rlib"], no deps ideally
│   └── src/
│       ├── lib.rs             # ABI shim ONLY: exports, host imports, host_stubs, thread_local BOARD
│       ├── board.rs           # board window: register decode, size 1/2/4 split, open bus, int line
│       ├── pcf8584.rs         # the controller model: S0–S3, status-bit state machine
│       ├── i2c.rs             # bus core: trait I2cDevice, transaction engine, fault injection
│       ├── fan.rs             # Fanny-compatible PWM regs + virtual fan (duty→RPM curve, stuck rotor)
│       ├── devices/
│       │   ├── mod.rs         # registry: address → device, per-device enable
│       │   ├── pcf8574.rs     # GPIO expander
│       │   ├── eeprom24.rs    # 24Cxx (size configurable; load/dump via resource + control)
│       │   ├── lm75.rs        # simple temp sensor (teaching example)
│       │   ├── ltc2990.rs     # temp/voltage monitor (the authentic resident)
│       │   └── pcf8583.rs     # RTC
│       ├── scenario.rs        # deterministic scripting: timeline resource, cck-keyed events
│       └── control.rs         # optional live control channel (host_sockets), feature/config-gated
├── tests/
│   └── copperline/
│       ├── run.sh             # tier-2 rig (copied/adapted from bridgeboard)
│       ├── machine.toml       # 68000/512K+512K profile, warp, [[zorro]] → ../../manifest/cpldicy.toml
│       ├── icy_probe.c        # guest probe: find board, PCF8584 smoke, prints SUB=x=PASS
│       ├── startup-sequence.txt
│       ├── scenarios/         # *.toml scenario fixtures (temp ramp, NAK fault, …)
│       └── run-oracle.sh      # tier-3: i2c.library + Aminet tools (needs nondistributable/)
├── nondistributable/          # git-ignored: Kickstart, i2c.library, FannyCtl, sensors tools
└── docs/
    ├── board-facts.md         # [P0] answers with citations
    ├── registers.md           # our register map as implemented
    └── tutorial.md            # Phase 3: the reference-plugin write-up
```

## 3. Architecture

### 3.1 ABI shim (`lib.rs`) — copy the bridgeboard pattern exactly

- Exports (wasm32-gated): `init()`, `read(off,size)->i32`,
  `write(off,size,value)`, `tick(cck)`, `int2()->i32` (or `int6`, per
  [P0]-3). No `reset` export — host re-instantiates; all state must be
  reachable from `init()` defaults.
- All mutable state in `thread_local! { static BOARD: RefCell<Board> }`
  — **no wasm globals / `static mut`** (save-states snapshot linear
  memory only).
- Host imports used: `log`, `config_get`, `resource_len`/`resource_read`;
  `sock_*` only when the live control channel is enabled. **No `dma`,
  no `net`, no `resolve`** — the board stays deterministic by default.
- Copy `config_get_string()` / `resource_get()` helpers and the
  `#[cfg(not(target_arch="wasm32"))] mod host_stubs` from the bridgeboard
  (`plugin/src/lib.rs:200-296`). Keep exports wasm32-gated or native
  `cargo test` silently breaks (documented hostsocket bug).

### 3.2 Board window (`board.rs`)

- Handle `size` 1, 2 **and 4** (split 4 into two 16-bit halves like
  `bridgeboard/plugin/src/shim/mod.rs:150`; never assume a fixed size —
  the hostsocket size==4 bug is the cautionary tale).
- Unmapped offsets return open bus (`0xFF...`), like `layout::OPEN_BUS`.
- Decode per [P0]-2: PCF8584 registers, fan registers, anything else the
  CPLD exposes.
- Interrupt is **level-sensitive and polled**: `int_line()` computes from
  PCF8584 PIN/interrupt-enable state (+ fan if applicable); the guest's
  register access clears the cause and the line drops. Model on
  `bridgeboard shim/registers.rs:107` + its unit tests.
- `tick(cck)` advances the I²C transaction engine and scenario timeline.
  I²C timing is virtual but **not instantaneous**: complete one bus phase
  per N ccks so PIN/BB status bits are observable in the right order and
  clock-stretch faults are expressible. N derived from the clock register
  setting (plumbed, even though wall-time is fake).

### 3.3 PCF8584 model (`pcf8584.rs`)

The datasheet is the specification. Model as an explicit state machine:
- Registers S0 (data), S1 (status/control, read vs write views differ),
  S2 (clock), S3/own-address, selected per the chip's ES/A0 scheme as
  decoded by the CPLD ([P0]-2).
- Serial state machine: idle → START → address+R/W → ACK → data bytes
  (TX and RX, with ACK/NAK from device or master) → STOP / repeated
  START. Status bits: PIN, BB (bus busy), LRB (last received bit), AAS,
  BER, and interrupt generation on PIN-low with ENI set.
- Every status-bit transition gets a native unit test; this file is where
  oracle failures will be debugged, so keep it pure (no ABI, no host
  imports) and exhaustively tested.

### 3.4 Virtual bus + devices (`i2c.rs`, `devices/`)

```rust
trait I2cDevice {
    fn start(&mut self, addr7: u8, read: bool) -> Ack;   // address phase
    fn write(&mut self, byte: u8) -> Ack;
    fn read(&mut self, last: bool) -> u8;                // master ACK/NAK signalled via `last`
    fn stop(&mut self);
    fn tick(&mut self, cck: u32);                        // e.g. RTC time advance
}
```
- Registry maps 7-bit address → device; devices individually enabled via
  manifest `[config]` (e.g. `pcf8574 = "0x20"`, `eeprom = "0x50:4096"`,
  `ltc2990 = "0x4C"`, empty string = absent). Defaults mirror the real
  CPLDIcy population (LTC2990 present at its wired address).
- Per-device and per-bus **fault knobs**, settable by scenario/control:
  address NAK, data NAK after byte k, clock stretch for M ticks,
  stuck-bus (SDA held low), EEPROM bit-flip mask.
- Each device is a small pure struct with unit tests against its
  datasheet register map. EEPROM initial contents loadable via a
  `type = "file"` option (arrives through `resource_read`).

### 3.5 Fan controller (`fan.rs`)

- Register interface per CPLDIcy sources ([P0]-4), FannyCtl the oracle.
- Virtual fan: configurable duty→RPM curve, spin-up lag in ticks, and a
  stuck-rotor fault; RPM feeds whatever tach mechanism the real board
  exposes, plus the control surface for assertions.

### 3.6 Scripting surface — two tiers (design decision)

CCP cannot poke device windows, and neither existing plugin exposes
runtime state to the host. So:

1. **Scenario files (primary, deterministic).** A TOML resource
   (manifest option `scenario`, `type = "file"`) with a cck-keyed event
   timeline:
   ```toml
   [[event]] at = 50_000_000   ; set = { device = "ltc2990", channel = "tint", celsius = 45.0 }
   [[event]] at = 90_000_000   ; fault = { device = "lm75", kind = "addr_nak" }
   ```
   Applied in `tick()`. Byte-identical replay for free; this is what the
   determinism success-criterion runs on. Assertions come back out via
   the guest probe printing observed values over serial, and via `log()`
   lines (`wasm[cpldicy]: ...`) grepped host-side.
2. **Live control channel (secondary, interactive/pytest).** Optional
   `control_listen = "127.0.0.1:0"` config; uses the `host_sockets`
   capability (adds `host_sockets = true` to the manifest **only when
   built/configured for it** — it breaks determinism, so default off).
   Newline-JSON commands polled non-blocking in `tick()`:
   `{"set":{"device":"ltc2990","channel":"tint","celsius":45}}`,
   `{"get":{"device":"fan"}}` → `{"duty":128,"rpm":2300}`.
   Implement in Phase 2 only if the AmiMQTT-style interactive test needs
   it; the scenario tier may cover everything. If Copperline upstream
   later grows a CCP "device mailbox" method, swap onto that.

### 3.7 Manifest (`manifest/cpldicy.toml`)

```toml
name = "CPLDIcy I2C"
zorro = 2
type = "wasm"
size = "64K"                     # [P0]-1
manufacturer = 0x....            # [P0]-1 (real CPLDIcy ID; 0x07DB hacker ID as placeholder)
product = ...
wasm = "../target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm"
int2 = true                      # [P0]-3

[config]
scenario = ""                    # optional timeline file
eeprom_image = ""                # optional 24Cxx initial contents
# per-device enables/addresses, fan curve params...

[[option]]
key = "scenario"; label = "Scenario script"; type = "file"
# ... one [[option]] per config key so the launcher renders a panel
```

## 4. Testing strategy (three tiers)

1. **Native unit tests** (`cargo test`, no emulator): PCF8584 state
   machine, each device model, fan curve, register decode, scenario
   parsing. The bulk of correctness lives here.
2. **Headless conformance rig** (`tests/copperline/run.sh`): freestanding
   m68k probe (no OS deps beyond boot) pokes the board window directly —
   AutoConfig identity visible, PCF8584 register access, one full
   master-TX and master-RX transaction against the PCF8574 with
   interrupt observed, fan duty write/readback. Prints `SUB=x=PASS`,
   grepped after `copperline --config machine.toml --noaudio --serial
   stdout --benchmark-until 20`. Runs with bundled AROS (no Kickstart
   needed) — this is the CI-able tier.
3. **Oracle tier** (`tests/copperline/run-oracle.sh`, needs
   `nondistributable/`): boot a Workbench-ish environment from a
   `[[filesys]]` host-dir mount (hostsocket's bsdsocktest pattern — logs
   land straight on the host FS), run unmodified `i2c.library` + an
   Aminet bus-scan tool + simplesensors/Sensei + FannyCtl. Not CI by
   default; scripted and greppable locally.

## 5. Phases

### Phase 0 — reading & facts (evenings; no code)
- Read `docs/zorro.md` + `wasmboard.rs` doc comment end-to-end.
- Read CPLDIcy repo + datasheets; fill `docs/board-facts.md` ([P0] 1–6).
- Fetch oracle binaries into `nondistributable/`; note versions.
- Courtesy heads-up email to Henryk Richter (user sends, not automated).
- Confirm toolchain: `rustup target add wasm32-unknown-unknown`,
  `m68k-amigaos-gcc`, `xdftool`, a Copperline build. Smoke-run the
  bridgeboard's `tests/copperline/run.sh` once to prove the rig works
  on this machine before blaming our own code later.

### Phase 1 — board core (1 weekend) — ✅ DONE, oracle-validated 2026-08-16
**Gate: unmodified `i2c.library` + one Aminet tool scan the bus and
toggle the PCF8574 under Copperline.** — **met**, see `docs/board-facts.md`
§8 item 6 and the README's "Oracle compatibility" table: `I2CScan` found
the PCF8574 via real address+W/address+R transactions, zero fixes needed.
1. Scaffold repo from template (workspace, Makefile, lib.rs shim with
   host stubs, empty board, manifest with [P0] identity). Board appears
   in AutoConfig; probe finds it. *(First `SUB=find_board=PASS`.)*
2. `pcf8584.rs` state machine + unit tests (TX path, then RX, then
   status-bit ordering, then error bits).
3. `i2c.rs` engine + `devices/pcf8574.rs`; wire into `board.rs` decode
   and `tick`; interrupt line.
4. Extend probe: full transactions + interrupt test → tier-2 green.
5. Oracle pass: `i2c.library` detect, bus scan, GPIO toggle. Fix the
   quirks the datasheet undersold (expected; keep fixes unit-tested).
   **Ran clean first try — no quirks surfaced, no fixes needed** (contrary
   to this section's own expectation; see board-facts.md §8 item 6 for
   the full transcript and the one benign observation that wasn't a bug).

### Phase 2 — peripherals, fan, scripting (1 weekend) — ✅ DONE, oracle-validated 2026-08-16
**Gate: closed thermal loop + deterministic replay + a fault fixture.** —
**met**, see `plugin/tests/flagship.rs`. Guest-oracle validation (item 4
below) also complete: FannyCtl + simplesensors both work unmodified.
1. `eeprom24.rs` (with image option), `lm75.rs`, `ltc2990.rs`,
   `pcf8583.rs` — each with unit tests.
2. `fan.rs` + virtual fan; FannyCtl oracle configures it.
3. `scenario.rs` + fault knobs. (Scenario fixtures live inline in
   `plugin/tests/flagship.rs` rather than separate
   `tests/copperline/scenarios/` files — see item 5's note.)
4. Oracle: simplesensors/Sensei read scripted LTC2990 values — **done**,
   `make oracle`; see README's oracle compatibility table.
5. The two flagship scripted tests — **done**, but as native Rust
   integration tests (`plugin/tests/flagship.rs`) rather than
   guest-probe/serial-output tests: they drive `Board` through its real
   I2C register protocol (the same sequence `i2c.library` issues) plus
   the scenario engine, which is deterministic and host-side by
   construction — a guest-side version of the same scenarios (a small
   m68k "fan-curve" C program reading LTC2990 and driving the fan via
   real `i2c.library` calls, under the tier-2 probe rig) remains a
   nice-to-have for even-higher-fidelity coverage, not a blocking gap,
   since the oracle pass already validates the real-driver protocol path
   independently and the flagship tests validate the scenario/physics
   logic independently.
   - **Thermal loop:** scripted LTC2990 temp ramp → "guest" (the test
     itself) reads it via real I2C protocol → fan curve computed → duty
     written → assert RPM rise.
   - **Fault fixture:** sensor NAK mid-run → handled visibly (no hang),
     bus recovers for other devices afterward.
6. Determinism check — **done**,
   `scripted_run_replays_byte_identically` in `plugin/tests/flagship.rs`.
7. `control.rs` live channel — **not built**; the scenario tier proved
   sufficient for everything attempted so far, including the oracle pass.

### Phase 3 — the reference-plugin write-up (part-weekend) — ✅ DONE (2026-08-16)
1. `docs/tutorial.md` — **done.** Self-contained, with its own worked
   example ("TimerPort", a one-register timer/interrupt board, distinct
   from CPLDIcy) covering manifest, ABI, host imports, determinism, and
   the three testing tiers. The example's code was independently
   compiled and its tests run to confirm it actually works, not just
   read-through-plausible. The docs-only acceptance test (a third party
   builds a *different* board from the tutorial alone) is satisfied by
   construction — TimerPort shares no logic with CPLDIcy, and the
   tutorial never tells the reader to go read `plugin/src/`.
2. README — **done** (landed alongside the oracle-validation work,
   docs/board-facts.md §7-adjacent): config snippet, oracle compatibility
   table, scenario format.
3. Tag release — **done**, `v0.1.0`.

## 6. Risks & notes for the implementer

- **PCF8584 quirks**: expect the oracle (i2c.library) to depend on exact
  PIN/BB timing and status readback order. Budget debug time in Phase 1
  step 5; every discovered quirk becomes a unit test before the fix.
- **Byte lanes**: a byte-wide chip on a 16-bit Zorro bus means registers
  likely appear at odd *or* even addresses only ([P0]-2). Get this wrong
  and i2c.library sees open bus — check first when detection fails.
- **Fuel budget**: 50M fuel per host call; keep `tick()` cheap (no
  allocation in the hot path; scenario events pre-sorted).
- **Fault state is sticky**: a trap (panic!) faults the board until bus
  reset — reads become 0xFFFFFFFF. If the board "disappears" mid-test,
  check Copperline's log for a wasm trap before debugging register logic.
- **No wasm globals**: all state via the thread_local RefCell, or
  save-states silently corrupt.
- **License hygiene**: nothing from the CPLDIcy repo (CC) or datasheets
  is copied into the tree — facts only, cited in `board-facts.md`.
  Oracle binaries stay in git-ignored `nondistributable/`.
- **Out of scope here**: AmiBake machine-block entry (`i2c = true`)
  lands with AmiBake, not this repo. Guest autoboot ROM / diag_vec —
  not needed, everything loads from disk.
