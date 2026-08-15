/* icy_probe.c -- on-target (m68k/AmigaOS) conformance test for the
 * CPLDIcy board window (docs/board-facts.md, docs/PLAN.md section 4 tier
 * 2), run against a real Copperline instance with manifest/cpldicy.toml
 * fitted. Validates the raw board contract directly -- AutoConfig
 * identity, the PCF8584 register window, and one full I2C master-tx +
 * master-rx round trip against the virtual PCF8574 -- the same way
 * plugin/src/board.rs's own native unit tests do, but from the real
 * 68000 side through Copperline's actual Zorro autoconfig and bus
 * timing, which the native unit tests cannot reach. This is NOT an
 * `i2c.library` conformance test -- that's tier 3 (docs/PLAN.md section
 * 4), needs the real library binary, and is not run by this script.
 *
 * Freestanding (-nostdlib -nostartfiles, no crt), modeled directly on
 * copperline-bridgeboard-plugin's tests/copperline/bridgeboard_probe.c:
 * real Kickstart crashes a normal linked -noixemul program's crt before
 * main() runs, and every string constant is a named `static char[]`
 * global rather than an inline literal, because this freestanding GCC
 * backend pools string literals at hunk offset 0 -- the program's entry
 * point.
 *
 * `_start` MUST be the very first function *definition* in this file
 * (only prototypes and globals before it), even with
 * `__attribute__((no_reorder))`: that attribute only stops GCC from
 * reordering relative to source order, it does not hoist `_start` ahead
 * of whatever function happens to be defined first. Learned the hard
 * way here: an earlier version defined the raw_put/raw_str/report
 * helpers before `_start`, and `no_reorder` faithfully preserved that
 * order -- `report` (not `_start`) ended up at .text offset 0, so
 * AmigaOS jumped straight into the middle of `report`'s prologue instead
 * of running any of the actual probe. `nm` on the bad binary showed
 * `__start` at offset 0x88, not 0. All real logic lives in
 * `test_main()`, defined after `_start`, exactly like
 * bridgeboard_probe.c's own layout.
 *
 * Board window layout constants below mirror plugin/src/board.rs and
 * plugin/src/pcf8584.rs exactly -- if those ever change, update both
 * sides together (there is no automatic check between them, same caveat
 * as the bridgeboard probe's own header).
 */

#include <exec/types.h>
#include <exec/execbase.h>
#include <libraries/expansion.h>
#include <proto/exec.h>
#include <proto/expansion.h>

struct ExecBase *SysBase;
/* proto/expansion.h only *declares* `extern struct ExpansionBase
 * *ExpansionBase;` -- with -nostdlib -nostartfiles there is no crt to
 * supply the actual storage that extern refers to, so this tentative
 * definition provides it (same as bridgeboard_probe.c). */
struct ExpansionBase *ExpansionBase;

static int test_main(void);

int __attribute__((no_reorder)) _start(void)
{
    return test_main();
}

/* -- board identity (manifest/cpldicy.toml / docs/board-facts.md §1).
 * i2c.library's own hardcoded scan value, not the CPLDIcy VHDL source's
 * -- see docs/board-facts.md §1's discrepancy note for why. */
#define BOARD_MANUFACTURER 5001
#define BOARD_PRODUCT 15

/* -- register offsets (plugin/src/board.rs): A0 area at 0, S1 at 2. Byte
 * accesses only -- the chip's data bus sits on D15-D8 (UDS) only, so a
 * byte access at these even offsets is what reaches it. */
#define REG_A0_AREA 0x00
#define REG_S1 0x02

/* -- S1 control bits (plugin/src/pcf8584.rs's `ctrl` module). */
#define CTRL_PIN 0x80
#define CTRL_ESO 0x40
#define CTRL_ENI 0x08
#define CTRL_STA 0x04
#define CTRL_STO 0x02
#define CTRL_ACK 0x01

/* -- S1 status bits (plugin/src/pcf8584.rs's `status` module). */
#define STATUS_PIN 0x80
#define STATUS_LRB_AD0 0x08
#define STATUS_BB 0x01

/* -- the virtual PCF8574 (plugin/src/board.rs's PCF8574_ADDRESS). */
#define PCF8574_ADDRESS 0x20

static char LIBNAME_EXPANSION[] = "expansion.library";
static char MSG_SUB[] = "SUB=";
static char MSG_EQ_PASS[] = "=PASS\n";
static char MSG_EQ_FAIL[] = "=FAIL\n";
static char MSG_RESULT_PASS[] = "RESULT=PASS\n";
static char MSG_RESULT_FAIL[] = "RESULT=FAIL\n";
static char MSG_END[] = "END\n";

static char NAME_FIND_BOARD[] = "find_board";
static char NAME_REGISTER_ROUNDTRIP[] = "register_roundtrip";
static char NAME_RESET_DEFAULTS[] = "reset_defaults";
static char NAME_MASTER_TX_ACK[] = "master_tx_ack";
static char NAME_MASTER_RX_ROUNDTRIP[] = "master_rx_roundtrip";

static void raw_put(char c)
{
    /* RawPutChar via SysBase LVO -516 -- reaches Copperline's
     * `--serial stdout` capture with no dos.library dependency, same
     * trick as bridgeboard_probe.c / hostsocket_test.c. */
    register long d0 __asm__("d0") = (unsigned char)c;
    register void *a6 __asm__("a6") = SysBase;
    __asm__ volatile("jsr -516(%%a6)" : : "r"(d0), "r"(a6)
                     : "d1", "a0", "a1", "cc", "memory");
}

static void raw_str(const char *s)
{
    while (*s)
        raw_put(*s++);
}

static void report(const char *name, int ok, int *fails)
{
    raw_str(MSG_SUB);
    raw_str(name);
    if (ok) {
        raw_str(MSG_EQ_PASS);
    } else {
        raw_str(MSG_EQ_FAIL);
        (*fails)++;
    }
}

static UBYTE reg_read(volatile UBYTE *base, ULONG off)
{
    return base[off];
}

static void reg_write(volatile UBYTE *base, ULONG off, UBYTE value)
{
    base[off] = value;
}

/* Poll S1 for PIN==0 (active/phase complete). Bounded loop -- Copperline's
 * virtual bus timing is fixed and small (CCK_PER_BYTE_PHASE), so this
 * should never need more than a handful of iterations; a real hang here
 * is a genuine board bug, not a timing fluke, so this deliberately does
 * NOT loop forever. */
static int wait_pin_active(volatile UBYTE *base)
{
    int i;
    for (i = 0; i < 100000; i++) {
        if ((reg_read(base, REG_S1) & STATUS_PIN) == 0)
            return 1;
    }
    return 0;
}

static int test_main(void)
{
    struct ConfigDev *cd;
    volatile UBYTE *base;
    int fails = 0;
    UBYTE s1;

    SysBase = *(struct ExecBase **)4UL;

    ExpansionBase = (struct ExpansionBase *)OpenLibrary((CONST_STRPTR)LIBNAME_EXPANSION, 0);
    if (!ExpansionBase) {
        raw_str(MSG_SUB);
        raw_str(NAME_FIND_BOARD);
        raw_str(MSG_EQ_FAIL);
        raw_str(MSG_RESULT_FAIL);
        raw_str(MSG_END);
        return 1;
    }

    cd = FindConfigDev(NULL, BOARD_MANUFACTURER, BOARD_PRODUCT);
    report(NAME_FIND_BOARD, cd != NULL, &fails);
    if (!cd) {
        raw_str(MSG_RESULT_FAIL);
        raw_str(MSG_END);
        CloseLibrary((struct Library *)ExpansionBase);
        return 1;
    }
    base = (volatile UBYTE *)cd->cd_BoardAddr;

    /* Reset defaults: PIN=1, BB=1, everything else 0 -- board.rs's
     * even_word_offset_0_reaches_the_a0_area_and_offset_2_reaches_s1
     * test asserts the same thing natively. */
    s1 = reg_read(base, REG_S1);
    report(NAME_RESET_DEFAULTS, s1 == (STATUS_PIN | STATUS_BB), &fails);

    /* Register round-trip through the A0 area (S0' while ESO=0). */
    reg_write(base, REG_A0_AREA, 0x55);
    report(NAME_REGISTER_ROUNDTRIP, reg_read(base, REG_A0_AREA) == 0x55, &fails);

    /* Select S0 for transfers (ESO=1), matching i2c.library's own init
     * sequence (docs/board-facts.md §4) -- required before an address
     * write below reaches the shift register. */
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_ACK);

    /* Master-transmit: address the virtual PCF8574, then write one byte
     * to its output latch. */
    reg_write(base, REG_A0_AREA, (PCF8574_ADDRESS << 1)); /* address+W */
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STA | CTRL_ACK);
    wait_pin_active(base);
    s1 = reg_read(base, REG_S1);
    report(NAME_MASTER_TX_ACK, (s1 & STATUS_LRB_AD0) == 0, &fails);

    reg_write(base, REG_A0_AREA, 0xA5); /* data byte */
    wait_pin_active(base);
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK); /* STOP */

    /* Master-receive: read the same byte back. */
    reg_write(base, REG_A0_AREA, (PCF8574_ADDRESS << 1) | 1); /* address+R */
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STA | CTRL_ACK);
    wait_pin_active(base);
    (void)reg_read(base, REG_A0_AREA); /* dummy read: arms the first byte */
    wait_pin_active(base);
    {
        UBYTE readback = reg_read(base, REG_A0_AREA);
        reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK); /* STOP */
        report(NAME_MASTER_RX_ROUNDTRIP, readback == 0xA5, &fails);
    }

    if (fails == 0) {
        raw_str(MSG_RESULT_PASS);
    } else {
        raw_str(MSG_RESULT_FAIL);
    }
    raw_str(MSG_END);

    CloseLibrary((struct Library *)ExpansionBase);
    return fails == 0 ? 0 : 1;
}
