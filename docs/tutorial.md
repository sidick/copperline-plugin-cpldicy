# Writing a Copperline Zorro plugin: a worked tutorial

This tutorial teaches the Copperline WASM Zorro plugin mechanism from
scratch, using a complete worked example — **TimerPort**, a fictitious
one-register Zorro II board that arms a one-shot delay and raises an
interrupt when it expires. It's not CPLDIcy (this repository's actual
board): the two boards don't share a byte of logic. Everything you need
to build TimerPort — or your own, different board — is in this document.
You should not need to read this repository's own `plugin/src/` to
follow along.

If you get through this and want to see a larger worked example, this
repository's own `plugin/src/` (a PCF8584 I2C controller with several
attached devices) is one — but it's an example of applying these same
concepts at larger scale, not a prerequisite for understanding them.

## 1. What you're building

A Copperline Zorro plugin is a small WebAssembly module plus a TOML
manifest. Copperline loads the manifest, autoconfigures a board with the
identity it declares, and routes every CPU read/write that lands in the
board's address window into your WASM module's exported functions. Your
module owns whatever register/memory model you want behind that window,
and can optionally raise an Amiga interrupt line and tick along with
emulated time.

TimerPort's whole behavior: writing any byte to its one register arms a
timer; some emulated cycles later, the timer fires, raises INT2, and
sets a "expired" flag; reading the register returns and clears that
flag. That's small enough to hold in your head, and touches every piece
of the ABI you'll actually use: register decode, `tick`, and an
interrupt line.

## 2. Prerequisites

- `rustup target add wasm32-unknown-unknown`
- A Copperline build (`copperline --version` should work)
- `xdftool` (`pip install amitools`) if you want the headless
  conformance-probe tier of testing (section 9)
- Optionally, an m68k cross-GCC (`m68k-amigaos-gcc`, e.g. via
  [bebbo's amiga-gcc](https://github.com/bebbo/amiga-gcc) or a Docker
  image built from it) — only needed for that same tier

## 3. Repo layout

A minimal plugin repo looks like this:

```
timerport-plugin/
├── Cargo.toml              # workspace
├── manifest/
│   └── timerport.toml      # AutoConfig identity + config schema
├── plugin/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs          # the whole board, for something this small
└── tests/
    └── copperline/         # optional: headless conformance rig
```

Workspace `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["plugin"]

[profile.release]
opt-level = "z"
lto = true
panic = "abort"
strip = true
```

`plugin/Cargo.toml`:

```toml
[package]
name = "timerport-plugin"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
# "cdylib" is what actually ships (the wasm32-unknown-unknown module).
# "rlib" is added so `cargo test` (native target) can link a normal test
# harness against this crate -- without it, `cargo test` silently exits
# before running anything.
crate-type = ["cdylib", "rlib"]

[dependencies]
```

## 4. The manifest

The manifest is the *only* place AutoConfig identity lives — never in
code. Minimal example:

```toml
name = "TimerPort"
zorro = 2
type = "wasm"
size = "64K"          # legal Zorro II sizes: 64K,128K,256K,512K,1M,2M,4M,8M

manufacturer = 0x07DB  # the conventional "hacker/prototype" ID for
                        # homemade boards without a real registered one
product = 1

wasm = "../target/wasm32-unknown-unknown/release/timerport_plugin.wasm"
int2 = true             # declares you'll use the int2 export/interrupt line
```

The `wasm` path resolves relative to the manifest file's own directory.
`manufacturer`/`product` together are what a guest's `FindConfigDev()`
matches against — pick real values only if you hold a registered
manufacturer ID; otherwise `0x07DB` is the community convention.

Every capability beyond the always-available host imports (`log`,
`config_get`, `resource_*`) needs to be declared here too: `int2`/`int6`
(interrupt lines), `dma` (chip-memory DMA access), `net`/`resolve`/
`host_sockets` (networking — these break save-state determinism, so only
declare them if you genuinely need them). TimerPort only needs `int2`.

## 5. The plugin ABI

Your WASM module exports whatever subset of these it needs — only
`memory` is mandatory:

| Export | Signature | Called |
|---|---|---|
| `memory` | (linear memory) | required |
| `init` | `() -> ()` | once, after instantiation, before any transaction |
| `read` | `(off: i32, size: i32) -> i32` | on a CPU read landing in your window |
| `write` | `(off: i32, size: i32, value: i32) -> ()` | on a CPU write landing in your window |
| `tick` | `(cck: i32) -> ()` | every bus tick, with elapsed emulated cycles |
| `int2` | `() -> i32` | polled every tick; non-zero = line asserted |
| `int6` | `() -> i32` | same, for the other interrupt line |

Things that surprise people the first time:

- **There's no `reset` export.** A bus reset re-instantiates your module
  from scratch (fresh linear memory, `init()` called again). Design your
  state so `init()`'s defaults are your reset defaults — there's no
  other reset hook to write.
- **`size` is 1, 2, or 4, and nothing upstream splits it for you.** A
  68000's word or long access into your window arrives as one `read`/
  `write` call with that size; decompose it yourself if your registers
  are narrower than the access. `off` is a byte offset; multi-byte
  values are big-endian, right-aligned in the `i32`.
- **An absent `read` reads as open bus (`0xFFFFFFFF`), not zero.** Same
  for out-of-window offsets within your declared size. Don't special-case
  this — just don't implement registers you don't have, and let
  unmapped offsets fall through to a default `0xFF` return.
- **Interrupts are level-sensitive and polled, not raised.** There's no
  "assert interrupt now" call. Hold `int2()` returning non-zero for as
  long as the condition is true; the host samples it every tick and
  handles delivery. It naturally follows that your interrupt condition
  needs to be something `int2()` can recompute (or read back) at any
  moment, not a one-shot event you'd otherwise "fire".
- **A trap faults your board, sticky, until reset.** A panic, an
  out-of-bounds access, anything that traps the WASM module marks it
  faulted: `read` returns open bus, `write`/`tick` no-op, `int2`/`int6`
  report deasserted — all silently, until the guest resets the bus. If
  your board mysteriously "disappears" mid-test, check the host log for
  a trap before debugging your register logic.
- **All state must live in linear memory, not WASM globals.**
  Save-states snapshot linear memory only. A `static mut` or any state
  held in a WASM global silently doesn't survive a save/load round trip.
  The standard pattern (used below) is a `thread_local!` `RefCell`
  holding your board struct.

## 6. Host imports

Available via `#[link(wasm_import_module = "env")]` under module `env`:

```rust
extern "C" {
    fn log(ptr: i32, len: i32);
    fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn resource_len(key_ptr: i32, key_len: i32) -> i32;
    fn resource_read(key_ptr: i32, key_len: i32, off: i32, out_ptr: i32, out_cap: i32) -> i32;
    // fn dma_read(addr: i32, ptr: i32, len: i32);       // needs `dma` in the manifest
    // fn dma_write(addr: i32, ptr: i32, len: i32);      // needs `dma` in the manifest
}
```

`config_get`/`resource_len`/`resource_read` read your manifest's
`[config]` defaults and any per-board `config = {...}` override, and
`type = "file"` resource options, respectively. Both return `-1` if the
key/resource is absent. `config_get`/`resource_read` truncate to your
buffer but still return the *untruncated* length — check the return
value if you care about truncation.

## 7. Worked example: TimerPort

The complete plugin, `plugin/src/lib.rs`:

```rust
use core::cell::RefCell;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn log(ptr: i32, len: i32);
}
#[cfg(not(target_arch = "wasm32"))]
mod host_stubs {
    #[allow(unused_variables)]
    pub unsafe fn log(ptr: i32, len: i32) {}
}
#[cfg(not(target_arch = "wasm32"))]
use host_stubs::*;

fn host_log(msg: &str) {
    unsafe { log(msg.as_ptr() as i32, msg.len() as i32) }
}

/// How long an armed timer takes to fire, in emulated bus cycles. Purely
/// illustrative -- pick whatever's observable in a test without being
/// glacially slow.
const TIMER_TICKS: u32 = 200;

struct Board {
    armed: bool,
    remaining: u32,
    expired: bool,
}

impl Board {
    fn new() -> Self {
        Self { armed: false, remaining: 0, expired: false }
    }

    /// Any write arms the timer, regardless of the byte value -- this
    /// board only has one thing to do.
    fn write(&mut self, _off: u32, _size: u32, _value: u32) {
        self.armed = true;
        self.remaining = TIMER_TICKS;
        self.expired = false;
    }

    /// Bit 0 of the one register reports (and clears) "expired".
    fn read(&mut self, off: u32, _size: u32) -> u32 {
        if off != 0 {
            return 0xFF; // open bus outside the one register we implement
        }
        let bit = u32::from(self.expired);
        self.expired = false;
        bit
    }

    fn tick(&mut self, cck: u32) {
        if !self.armed {
            return;
        }
        if cck >= self.remaining {
            self.armed = false;
            self.expired = true;
            host_log("timerport: fired");
        } else {
            self.remaining -= cck;
        }
    }

    fn int2_asserted(&self) -> bool {
        self.expired
    }
}

thread_local! {
    static BOARD: RefCell<Board> = RefCell::new(Board::new());
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn init() {
    host_log("timerport: init");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_board_reads_zero_and_has_no_interrupt() {
        let mut board = Board::new();
        assert_eq!(board.read(0, 1), 0);
        assert!(!board.int2_asserted());
    }

    #[test]
    fn arming_then_ticking_past_the_delay_sets_expired_and_raises_int2() {
        let mut board = Board::new();
        board.write(0, 1, 0xFF); // value is ignored -- any write arms it
        board.tick(TIMER_TICKS - 1);
        assert!(!board.int2_asserted(), "shouldn't have fired yet");

        board.tick(1); // crosses the threshold
        assert!(board.int2_asserted());
        assert_eq!(board.read(0, 1), 1);
    }

    #[test]
    fn reading_clears_the_expired_flag_and_deasserts_int2() {
        let mut board = Board::new();
        board.write(0, 1, 0);
        board.tick(TIMER_TICKS);
        assert!(board.int2_asserted());

        board.read(0, 1);
        assert!(!board.int2_asserted(), "read should have cleared it");
    }

    #[test]
    fn offsets_beyond_the_one_register_read_as_open_bus() {
        let board_read = Board::new().read(4, 1);
        assert_eq!(board_read, 0xFF);
    }
}
```

`manifest/timerport.toml` is exactly the manifest from section 4.

Build and test it exactly like any other Rust crate:

```sh
cargo test                                            # native unit tests
cargo build --release --target wasm32-unknown-unknown # the shipped module
```

## 8. Determinism

If your board's state lives entirely in the `Board` struct owned by the
`thread_local!` (as above), determinism and save-state support come for
free — Copperline snapshots linear memory, and your struct lives there.
The only way to break this:

- Reaching for a WASM global or `static mut` instead of the
  `thread_local!`/struct pattern.
- Declaring `net`/`resolve`/`host_sockets` in the manifest and actually
  using them — real-world I/O is inherently non-deterministic. Don't
  declare capabilities you don't need.

## 9. Testing your board

Three tiers work well, in increasing order of realism:

1. **Native unit tests** (shown inline above) — the bulk of your
   coverage should live here. Fast, no emulator needed.
2. **A headless conformance probe** — a freestanding m68k program that
   pokes your board's window directly under real Copperline AutoConfig
   and bus timing, run via `copperline --config machine.toml --serial
   stdout --benchmark-until N` and grepped for markers it prints. This
   catches things unit tests structurally can't: whether your manifest's
   identity actually autoconfigures, whether your byte-lane/size
   handling survives a real 68000 access pattern.

   The one gotcha that will cost you an afternoon if you don't know
   about it up front: **a freestanding, `-nostdlib -nostartfiles`
   program's entry point must be the *first function definition* in the
   source file**, full stop. `__attribute__((no_reorder))` on your
   `_start` function does *not* fix this if you define helper functions
   above it in the file — that attribute only stops GCC from reordering
   relative to *source* order, so whatever you defined first still ends
   up at `.text` offset 0, and the AmigaOS loader jumps straight into
   the middle of that function's prologue instead of your code, with no
   crash report, just silence. Fix: put `_start` (calling a `test_main`
   defined afterward) as the very first thing after your global
   variable declarations, before any helper function *bodies* (forward
   declarations are fine anywhere).
3. **A guest-oracle pass**, if real-world driver software exists for
   boards like yours — boot it, unmodified, against your emulation. This
   is the strongest validation there is, and often the only way to
   discover behavior a datasheet undersells. (This repository's own
   `tests/copperline/run-oracle.sh` is a full worked example of this
   tier, if you want to see the pattern for something more elaborate
   than TimerPort.)

## 10. Common pitfalls checklist

- [ ] `_start` is the first function *definition* in your probe's source
      file (section 9).
- [ ] `read`/`write` handle `size` 1, 2, *and* 4 — don't assume the CPU
      only ever does byte accesses just because your registers are byte-
      wide.
- [ ] Unmapped offsets return open bus (a nonzero default, not 0), and
      writes to them are silently dropped.
- [ ] No `static mut` or WASM globals anywhere in your board state.
- [ ] `int2`/`int6` are *level*, recomputed from current state on every
      poll — not a one-shot "I raised it once" flag that never clears.
- [ ] You didn't declare `dma`/`net`/`resolve`/`host_sockets` in the
      manifest unless you actually call the corresponding host imports —
      each one you *do* declare and use costs you save-state
      determinism.
- [ ] `tick()` stays cheap — no allocation, no unbounded loops. Every
      host call refuels a finite fuel budget; a single expensive `tick`
      can trap (and fault) your own board.

## 11. Where to go from here

TimerPort is deliberately as small as a board can usefully be. Real
boards tend to grow in a few directions this tutorial didn't need to
cover, all demonstrated at larger scale in this repository's own
`plugin/src/`:

- **Multiple sub-devices behind one controller chip** (`plugin/src/i2c.rs`,
  `plugin/src/devices/`) — a trait per device, a registry the controller
  dispatches through.
- **Configurable behavior via the manifest's `[config]`/`[[option]]`
  schema** (`manifest/cpldicy.toml`, `plugin/src/board.rs`'s
  `BoardConfig`) — reading `config_get`/`resource_read` to build your
  board differently per deployment.
- **Deterministic scripted scenarios** (`plugin/src/scenario.rs`) — a
  cck-keyed event timeline applied from `tick()`, so host-side tests can
  script "what the physical world is doing" and assert what the guest
  does about it, byte-identically on every replay.

None of that changes anything in sections 1–9 above — it's the same
manifest, the same ABI, the same `thread_local!`/`RefCell` pattern,
just applied to more registers and more devices.
