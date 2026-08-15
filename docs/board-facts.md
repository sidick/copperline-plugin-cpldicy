# Board facts (Phase 0)

Consolidated findings for the [P0] items in `PLAN.md` §1. Facts only,
never copied source — the CPLDIcy repo is CC-licensed and read-not-vendored;
the datasheets are proprietary references, likewise cited not copied.
Everything here is quoted/paraphrased from primary sources with citations,
gathered 2026-08-15.

Two low-confidence items are flagged explicitly below — do not build
register-timing-critical logic on them without a second check against a
real board or the CPLD source directly.

---

## 1. AutoConfig identity (manifest/cpldicy.toml)

Source: `Logic/IcyCPLD.vhd` autoconfig process, `gitlab.com/HenrykRichter/cpldicy`.

| Field | Value | Note |
|---|---|---|
| Manufacturer ID | **0x0A1C** (2588 decimal) | "a1k.org", same as the original M. Böhmer ICY board |
| Product ID | **0x0F** (15 decimal) | matches `i2c.library`'s hardcoded scan (see §6) |
| Board type | Zorro II, no add-memory, no boot ROM | `Er_Type = 1100` nibble |
| Board size | **64 KB** | `Er_BoardSize = 0001` |
| Serial | `0x1CE1CEBB` (current rev 0.04); `0x1CEDECAF` (rev 0.03); `0x1CECAFFE` (rev 0.01/0.02) | cosmetic; any fixed value is fine for the emulation |

**Note:** `i2c.library`'s AutoConfig scan uses manufacturer **5001** / product **15**
(decimal) per its source (`bcu.i`) and the independent Linux `i2c-icy`
driver (`ZORRO_ID(VMC,15,0)`) — **0x0A1C = 2588, not 5001.** These don't
match. Resolve this discrepancy in Phase 1 before trusting either value —
most likely one research pass read a different manufacturer-ID encoding
(Zorro IDs are sometimes quoted as the raw AutoConfig nibbles vs. the
final resolved 16-bit ID, and 5001 might be M. Böhmer's *original* ICY
manufacturer ID with CPLDIcy deliberately reusing it while 0x0A1C is a
misread of the VHDL comment, or vice versa). **Action for Phase 1:** grep
`i2c.library`'s actual detection call again and cross-check against a
real `IcyCPLD.vhd` build/simulation output (or ask Henryk Richter
directly, which is on the agenda anyway per the courtesy heads-up). Ship
whichever value makes `i2c.library`'s unmodified `FindConfigDev` actually
find the board — that behavioral test is definitive regardless of which
research summary is right.

## 2. Register map / byte-lane decoding

Source: `Logic/IcyCPLD.vhd`, `Logic/IcyCPLD.ucf`, `PCB/CPLDICY.kicad_pcb`,
`PCB/zorro2port_exp.lib`.

- The CPLD decodes only the board's 64 KB page (`nCS`/`nDTACK`); it does
  **not** decode PCF8584 sub-registers — `BA1 <= A(1)` passes Zorro
  address bit A1 straight through to the PCF8584's own register-select
  pin.
- **Word offset $00 = PCF8584 S0** (data register); **word offset $02 =
  PCF8584 S1** (control/status). S2 (clock) and S3 (own address) are
  reached indirectly via S1's ES1 bit, exactly per stock PCF8584
  behavior (see §3) — not separately memory-mapped.
- **Byte lane: the PCF8584's 8-bit data bus sits on D15-D8 (upper byte
  only)**. The CPLD's `nDS` input is wired only to Zorro `/UDS`; `/LDS`
  is unconnected. Board decode logic:
  `board.rs` must place register content in bits 15-8 for word/byte
  accesses and treat `/LDS`-only (odd-address byte) accesses as
  not reaching the chip at all (open bus).
- **Low confidence:** a second CPLD input, VHDL port `CTRL` (pin P35,
  net `/CTRL1`, pulled up via R2), feeds `nDTACK` generation
  (`nDTACK <= '0' when ... CTRL='0' or (RW='1' and ICY_ENABLED='0')`).
  Exact PCF8584 pin identity not confirmed from text sources alone. For
  a behavior-only (non-timing-accurate) emulation this is likely
  irrelevant — just assert DTACK immediately like a normal register
  access — but flag if wait-state-accurate timing ever becomes a goal.

## 3. Interrupt line

Source: `PCB/CPLDICY.kicad_pcb` net `/PCFINT`, `PCB/zorro2port_exp.lib`.

- **INT2 (PORTS) only** — the PCF8584's open-drain `INT` pin is wired
  directly to Zorro connector pin 19 (`/INT2`), bypassing the CPLD
  entirely. INT6 is not used.
- The CPLD's `nBERR` output is permanently tri-stated (`'Z'`) — no bus
  error signaling from this board.
- **No CPLD-side ack logic.** Interrupt clears exactly per stock PCF8584
  behavior: reading S1 (status) clears the pending condition that drives
  INT low, same as any other PCF8584-based board. Implement per the
  PIN-bit semantics in §4 below — `int2()` export should assert exactly
  when the real chip's INT pin would (PIN=0 and ENI=1), nothing CPLDIcy-specific.

## 4. PCF8584 controller model (chip-level, board-agnostic)

Source: NXP/Philips PCF8584 datasheet (educypedia.org mirror; also
alldatasheet.com, manualzz.com), cross-checked against Linux
`drivers/i2c/algos/i2c-algo-pcf.c`/`.h`.

### Registers (2 addresses × ES1/ES2 mux when ESO=0)

- **S1** (`A0=1`, our offset $02): control (write) / status (read), same
  address, physically separate write/read latches.
- **A0=0** (our offset $00), muxed by ES1/ES2 while ESO=0:
  - `ES1=0,ES2=0` → **S0'** own-address register (init only; bits 7..1 =
    7-bit address, left-shifted by 1 vs. the S0 comparison value)
  - `ES1=0,ES2=1` → **S3** interrupt vector (default `0x00`; unused by
    this board per §3)
  - `ES1=1,ES2=0` → **S2** clock register: bits `0 0 0 S24 S23 S22 S21 S20`;
    `S22-S24` = input clock select (Table 3), `S20-S21` = SCL output
    frequency (Table 2: 00→90kHz, 01→45kHz, 10→11kHz, 11→1.5kHz); no
    documented reset default, treat as undefined until firmware writes it.
  - When `ESO=1`, `A0=0` is always **S0** (data shift register / read
    buffer — write and read sides are physically distinct, see the
    "dummy read" rule below).

### Control register S1 (write): `PIN ESO ES1 ES2 ENI STA STO ACK`

- **PIN** (0x80) — write 1 resets all status bits to 0 (software-reset trick).
- **ESO** (0x40) — 0=register-init-mux mode, 1=serial-I/O mode (S0 active).
- **ES1/ES2** (0x20/0x10) — register mux select (see above); ES1 also
  selects long-distance 4-wire mode when ESO=1.
- **ENI** (0x08) — enable external INT pin.
- **STA/STO** (0x04/0x02) — START/STOP/REPEAT-START/chaining per the
  instruction table below.
- **ACK** (0x01) — auto-ACK received bytes; clear before the last byte
  in master-receive to generate the final NACK.

STA/STO instruction table:

| STA | STO | Function |
|---|---|---|
| 1 | 0 | START (or REPEAT START if already MST/TRM) |
| 0 | 1 | STOP → SLV/REC |
| 1 | 1 | DATA CHAINING (STOP then START+address, bus never released) |
| 0 | 0 | NOP |

Canonical write bytes (from Linux driver, matches datasheet flowcharts):
`START=0xC5, STOP=0xC3, REPSTART=0x45, IDLE=0xC1`.

### Status register S1 (read): `PIN [INI] STS BER AD0/LRB AAS LAB BB`

- **PIN** (0x80) — pending-interrupt-not; full semantics below.
- **bit6/INI** (0x40) — artifact of ESO=0 readback state, not a true status flag.
- **STS** (0x20) — slave-mode STOP detected (slave-receiver only).
- **BER** (0x10) — bus error (misplaced START/STOP); forces BB→1, PIN→0.
- **AD0/LRB** (0x08) — if AAS=0: LRB = last received bit (0 = ACK'd);
  if AAS=1: AD0 = 1 if General Call address matched.
- **AAS** (0x04) — addressed as slave (own address or General Call matched).
- **LAB** (0x02) — lost arbitration (multi-master).
- **BB** (0x01) — **inverted sense**: 0 = bus busy, 1 = bus free.

### PIN semantics (the crux of correct emulation)

- Writing STA=1 sets PIN=1 (inactive) immediately.
- **Transmitter mode:** writing S0 sets PIN=1 immediately; hardware
  resets PIN→0 automatically once the byte + ACK finishes on the wire.
  INT asserts (if ENI=1) exactly on that PIN→0 transition.
- **Receiver mode:** PIN→0 automatically when a byte completes
  (post-ACK/NACK), and SCL is held low (clock-stretched) until firmware
  reads S0, which sets PIN→1 and releases SCL — each S0 read both
  consumes the received byte and re-arms the next one.
- BER forces PIN→0 at any time; in slave-receiver mode an external STOP
  also forces PIN→0 (and sets STS).
- Writing PIN=1 directly is a documented full status-reset trick (used
  for init sync and LAB recovery).

**Master-transmit sequence:** poll S1 for BB=1 (free) → write address+W
to S0 → write S1=0xC5 (START) → poll PIN=0 → check LRB=0 (ACK'd) → for
each data byte: write S0 (sets PIN=1), poll PIN=0, check LRB → write
S1=0xC3 (STOP) after the last byte.

**Master-receive sequence — dummy-read quirk:** write address+R to S0 →
write S1=0xC5 (START) → poll PIN=0, check LRB (address ACK'd) → **dummy
read S0** (discarded; this read is what re-arms SCL to start clocking in
the first real byte — 1-byte pipeline lag) → poll PIN=0, read S0 (byte
N-1, re-arms byte N) → repeat → before the *last* byte, clear ACK (write
S1 with ACK=0, e.g. `0x40`) so the final incoming byte gets NACK'd → poll
PIN=0 → write S1=0xC3 (STOP) → read S0 once more for the final byte
(buffer fetch only, no bus activity).

### Reset defaults

All S1 flags reset to 0 **except PIN and BB, which reset to 1**. S0'=0x00,
S3=0x00 (S0'=0 ⇒ monitor/passive mode until firmware programs a real
own address). S2 has no documented reset default — treat as undefined.

### Detection/compatibility requirements for `i2c.library`

From `i2c.library`'s `bcu.s` driver behavior (Linux driver corroborates
generic parts):
- Init probe: write PIN alone (S1=0x80), read back, require
  `(status & 0x7f) == 0`.
- Writes own address `0x55` to S0' (bus address becomes `0xAA` effective).
- Sets S2 from an internal 4-entry clock table (exact byte values not
  fully confirmed; Linux's `i2c-icy` uses `0x1c` as its normal S2 write —
  a safe default to accept).
- Interrupt-driven above a poll-size threshold, using INT2/PORTS +
  VBlank-based software timeout (~50Hz ticks) — **the emulated INT2 line
  must actually assert correctly and promptly on PIN→0 with ENI set, or
  the driver stalls until its real-time VBlank timeout and force-issues
  STOP.**
- Polls BB (NBB) after STOP with the same VBlank timeout — BB must clear
  promptly on START and set promptly on STOP.
- Standard dummy-read receive pattern, NAK generated via ACK=0 before
  the final read — no ICY-specific deviation found.

## 5. PWM fan controller — MAX31760 ("Fanny"-compatible)

Source: `PCB/CPLDICY.sch` (component U5), `i2csensors/Fanny/src/MAX31760.h`,
`i2csensors/sensors/devs/Sensors/MAX31760_A0_Fanny.cfg`/`_A2_Fanny.cfg`.

- **Not memory-mapped through the CPLD at all** — it's a MAX31760 I2C
  chip on the shared bus, the *same chip* the standalone Fanny ISA card
  uses, addressed as another I2C slave. FannyCtl/the existing Fanny
  driver config works unmodified against it — this is real
  register-level compatibility, not board-level emulation.
- **I2C address:** likely **0xA0** (8-bit) — the Fanny driver's own
  default per `FannyCtl`'s README (`CHIP=DEV/K ... default: A0`). Two
  configs exist (0xA0, 0xA2) for boards with two chips; CPLDIcy has one
  chip. **Low confidence** — not directly confirmed against a real board;
  verify in Phase 1 against the oracle FannyCtl tool actually finding it.
- Register map (MAX31760 datasheet, confirmed via `MAX31760.h`):
  - `0x00-0x02` CR1/CR2/CR3 control (PWM mode, temp source, ramp rate)
  - `0x03` FFDC, `0x04` MASK, `0x05` IFR
  - `0x06-0x0F` temperature threshold registers
  - `0x10-0x17` 8-byte user EEPROM
  - `0x20-0x4F` 48-byte temp→PWM lookup table
  - **`0x50` PWMR** (ramp rate), **`0x51` PWMV** (PWM duty value)
  - **`0x52/0x53` TC1H/TC1L** — Fan1 tach count (16-bit)
  - **`0x54/0x55` TC2H/TC2L** — Fan2 tach count (16-bit)
  - `0x56-0x59` remote/local temperature
  - `0x5A` SR (status), `0x5B` EEX (EEPROM store)
- Temperature format: 10-bit signed, bit offset 1, 0.125°C/LSB.
- Tach format: 16-bit raw count; **RPM = 3,000,000 / raw_count** (assumes
  2 pulses/rev, 100kHz internal tach clock — the virtual fan model should
  invert this formula to synthesize a tach count from a target RPM).

## 6. LTC2990 temperature/voltage monitor

Source: LTC2990 datasheet Rev F (analog.com); `i2csensors/sensors/devs/Sensors/LTC2990.cfg`
(explicitly headed "configuration for ICYv2 board"); `PCB/CPLDICY.sch`.

- **I2C address: 0x98 (8-bit) = 0x4C (7-bit)** — the default/floating
  address-pin setting. (Note: the *separate* Fanny board's own LTC2990
  uses 0x9A/0x4D with a different channel layout — **do not reuse that
  config for CPLDIcy's on-board chip**.)
- Register map (power-on-reset clears all to 0x00):

| Addr | Name | Contents |
|---|---|---|
| 0x00 | STATUS | busy + per-channel ready flags |
| 0x01 | CONTROL | mode/format select |
| 0x02 | TRIGGER | write=trigger conversion, read=STATUS |
| 0x04/0x05 | T_INT | internal temp, 13-bit signed, 0.0625°C/LSB |
| 0x06/0x07 | V1 | 5V rail via 10k/10k divider, 0.61mV/LSB (per CPLDIcy's .cfg) |
| 0x08/0x09 | V2 | 12V rail via 30.1k/10k divider, 1.22mV/LSB |
| 0x0A/0x0B | V3/T_R2 | external NPN diode temp pair (13-bit, 0.0625°C/LSB) |
| 0x0C/0x0D | V4/T_R2 | (paired with V3 as the differential/temp channel) |
| 0x0E/0x0F | VCC | supply voltage, 14-bit, 305.18µV/LSB + 2.5V offset |

- **CPLDIcy's active channel config** (matches ICYv2, what simplesensors/
  Sensei expect out of the box): **Tint + VCC + V1(5V single-ended) +
  V2(12V single-ended) + V3/V4(external diode differential temp pair)**.
  CONTROL register init value per the `.cfg`: `0x18` family (mode bits
  select this specific single-ended-pair + external-diode-pair combo);
  exact bit decode not spelled out by the `.cfg` — decode against the
  datasheet's CONTROL register table in Phase 1 when implementing
  `devices/ltc2990.rs` (Mode Select b[4:3]/b[2:0] per the datasheet — the
  general register semantics are in this doc's "generic chip facts" below).
- Generic chip facts (any LTC2990, useful for the device model regardless
  of exact config bits):
  - STATUS (0x00): b0=Busy, b1=T_INT ready, b2=V1/TR1/V1-V2 ready,
    b3=V2 ready, b4=V3/TR2/V3-V4 ready, b5=V4 ready, b6=VCC ready.
  - CONTROL (0x01): b7=format (0=C,1=K), b6=repeat/single, b[4:3]=outer
    mode (00=Tint only [default], 01/10/11=channel pairs per b[2:0]),
    b[2:0]=channel-pair select (000=V1,V2,TR2 default; 111=V1,V2,V3,V4; etc.
    — full 8-entry table in the datasheet, needed verbatim when writing
    `ltc2990.rs`'s mode decode).
  - Temperature format: 13-bit two's complement, MSB byte has
    DATA_VALID(b7)/Sensor-Short(b6)/Sensor-Open(b5) flags + D[12:8], LSB
    byte = D[7:0]; T = D[12:0]/16.
  - Voltage format: 14-bit, MSB byte has DATA_VALID(b7)/Sign(b6)+D[13:8],
    LSB=D[7:0]; single-ended LSB=305.18µV, differential LSB=19.42µV.

## 7. Guest oracle software

- **`i2c.library`**: BSD-relicensed source at `github.com/Sakura-IT/i2clib`
  (`bcu.i`, `i2c.library.bcu.s` = the PCF8584/"bcu" driver used for ICY).
  First shipped ICY support in **v40** (Aminet `docs/hard/i2clib40.lha`,
  3 Jan 2000; driver internal version string "40.3 (30 Dec 99)"). Fetch
  this into `nondistributable/` for Phase 1 oracle testing.
- **FannyCtl**: from `i2csensors/Fanny/` — configures the MAX31760 curve;
  README documents its default chip address assumption (`A0`).
- **simplesensors / Sensei**: read via driver `.cfg` files matching the
  exact LTC2990/MAX31760 register configs above — the `.cfg` format
  itself (from `i2csensors/sensors/devs/Sensors/*.cfg`) may be worth
  fetching as a secondary spec for the device register maps.
- CPLDIcy's own README notes the CPLD's indirect PCF8584 implementation
  can have "occasional mis-configuration ... and timing issues" versus a
  real PCF8584 — i.e. even the real hardware isn't perfectly faithful to
  the datasheet; don't over-fit the emulation to timing nuances the real
  board itself doesn't guarantee.

## 8. Open items for Phase 1

1. **Resolve the manufacturer-ID discrepancy (§1)** — 0x0A1C (VHDL) vs.
   5001 decimal (`i2c.library`/Linux driver) — before writing the
   manifest. Ground truth = whichever value gets unmodified `i2c.library`
   to actually find the board.
2. Confirm the MAX31760 I2C address (0xA0 assumed, low confidence) against
   FannyCtl actually finding it.
3. Decode the LTC2990 CONTROL register's exact init byte from the
   `.cfg`'s `00001800` value against the datasheet's mode-select table
   when writing `devices/ltc2990.rs`.
4. The `CTRL`/DTACK-generation CPLD input (§2) — low confidence on which
   physical PCF8584 pin it is; irrelevant unless timing-accurate DTACK
   becomes a goal.
5. Courtesy heads-up to Henryk Richter is still a user action (per
   PLAN.md Phase 0), not something this research pass can do.
