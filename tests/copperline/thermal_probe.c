/* thermal_probe.c -- a real m68k guest driving the closed thermal loop
 * (docs/PLAN.md section 4 tier 2 extension, issue #8): the same
 * scenario `plugin/tests/flagship.rs`'s
 * `closed_thermal_loop_scripted_temperature_drives_fan_response` test
 * exercises host-side (LTC2990 Tint ramps from a scripted
 * `thermal-scenario.txt`, a simple fan curve computes a duty cycle,
 * that duty is written to the MAX31760, and the virtual fan's tach
 * registers are polled for a response) -- but from the real 68000 side
 * through Copperline's actual Zorro autoconfig and bus timing, which
 * that host-side test cannot reach. Freestanding, modeled directly on
 * `icy_probe.c` (see that file's header for the `_start`-must-be-first
 * discipline and why) -- this is its own guest driver, standing in for
 * FannyCtl/simplesensors the way flagship.rs's own module docs describe.
 *
 * Timing note: `tick(cck)` (docs/tutorial.md) is driven by Copperline
 * itself as real emulated bus cycles elapse while this probe's own code
 * runs -- there is no way for guest code to advance it directly, and no
 * dos.library Delay() available freestanding. So instead of guessing at
 * a cck-to-instruction ratio and blindly delaying, every wait below
 * polls a real I2C transaction in a bounded loop until the observed
 * value actually changes (or the loop's generous iteration budget runs
 * out), same discipline `wait_pin_active` (below) already uses for a
 * single register poll. Measured empirically against a real Copperline
 * run (the `DIAG=cool_temp16=` line `raw_long` prints, kept in since
 * it's what made the actual numbers below discoverable at all, not
 * something derivable by reasoning about the board model alone): plain
 * AmigaOS boot + `OpenLibrary("expansion.library")` + `FindConfigDev`
 * alone burns cck somewhere in the tens of millions before this probe's
 * first instruction even runs -- far more than `CCK_PER_BYTE_PHASE`-scale
 * reasoning from the board model alone would suggest.
 * `thermal-scenario.txt`'s ramp timestamp (100,000,000) and
 * `TEMP_POLL_MAX`/`FAN_POLL_MAX` below are both sized off that
 * measurement, with comfortable headroom on both sides: enough below the
 * ramp that the "still cool" baseline reliably observes the pre-ramp
 * value, and enough polling budget past it that the post-ramp poll
 * reliably crosses the threshold within a few real seconds.
 *
 * Board window layout constants mirror plugin/src/board.rs/pcf8584.rs
 * exactly, same caveat as icy_probe.c: no automatic check between them.
 */

#include <exec/types.h>
#include <exec/execbase.h>
#include <libraries/expansion.h>
#include <proto/exec.h>
#include <proto/expansion.h>

struct ExecBase *SysBase;
struct ExpansionBase *ExpansionBase;

static int test_main(void);

int __attribute__((no_reorder)) _start(void)
{
    return test_main();
}

/* -- board identity (manifest/cpldicy.toml / docs/board-facts.md §1). */
#define BOARD_MANUFACTURER 5001
#define BOARD_PRODUCT 15

/* -- register offsets (plugin/src/board.rs). */
#define REG_A0_AREA 0x00
#define REG_S1 0x02

/* -- S1 control bits (plugin/src/pcf8584.rs's `ctrl` module). */
#define CTRL_PIN 0x80
#define CTRL_ESO 0x40
#define CTRL_STA 0x04
#define CTRL_STO 0x02
#define CTRL_ACK 0x01

/* -- S1 status bits (plugin/src/pcf8584.rs's `status` module). */
#define STATUS_PIN 0x80
#define STATUS_LRB_AD0 0x08

/* -- the real board's own authentic residents (plugin/src/board.rs's
 * BoardConfig::default(), plugin/src/fan.rs). */
#define LTC2990_ADDRESS 0x4C
#define REG_TINT_MSB 0x04
#define MAX31760_ADDRESS 0x50
#define REG_PWMV 0x51
#define REG_TC1H 0x52
#define REG_TC1L 0x53

/* Generous bounded budgets -- see the timing note above. A single poll
 * iteration performs a full I2C transaction (several ~200-cck phases),
 * so these translate to comfortably more real elapsed cck than the
 * scripted scenario's own timestamps (thermal-scenario.txt) need. */
#define PIN_WAIT_MAX 100000
#define TEMP_POLL_MAX 2000000
#define FAN_POLL_MAX 2000000

static char LIBNAME_EXPANSION[] = "expansion.library";
static char MSG_SUB[] = "SUB=";
static char MSG_EQ_PASS[] = "=PASS\n";
static char MSG_EQ_FAIL[] = "=FAIL\n";
static char MSG_RESULT_PASS[] = "RESULT=PASS\n";
static char MSG_RESULT_FAIL[] = "RESULT=FAIL\n";
static char MSG_END[] = "END\n";

static char NAME_FIND_BOARD[] = "find_board";
static char NAME_COOL_BASELINE[] = "ltc2990_cool_baseline";
static char NAME_TEMP_RISES[] = "ltc2990_temperature_rises";
static char NAME_FAN_DUTY_ACK[] = "fan_duty_write_ack";
static char NAME_FAN_SPINS_UP[] = "fan_spins_up";
static char MSG_DIAG_COOL_TEMP16[] = "DIAG=cool_temp16=";

static void raw_put(char c)
{
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

/* Diagnostic only (tuning thermal-scenario.txt's timestamps against
 * real cck-per-instruction throughput) -- prints a signed decimal
 * number over serial with no libgcc/printf dependency. */
static void raw_long(LONG v)
{
    /* No `%`/`/` here either (see the fixed-point note near fan_curve
     * below) -- digit extraction via repeated subtraction instead. */
    char digits[12];
    int n = 0;
    ULONG uv, r, q;
    if (v < 0) {
        raw_put('-');
        uv = (ULONG)(-v);
    } else {
        uv = (ULONG)v;
    }
    if (uv == 0) {
        raw_put('0');
        return;
    }
    while (uv > 0 && n < 12) {
        r = uv;
        q = 0;
        while (r >= 10) {
            r -= 10;
            q++;
        }
        digits[n++] = (char)('0' + r);
        uv = q;
    }
    while (n > 0) {
        raw_put(digits[--n]);
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

static int wait_pin_active(volatile UBYTE *base)
{
    int i;
    for (i = 0; i < PIN_WAIT_MAX; i++) {
        if ((reg_read(base, REG_S1) & STATUS_PIN) == 0)
            return 1;
    }
    return 0;
}

/* Full master-transmit: address + all of `bytes`, then STOP. Returns 0
 * (and still issues STOP, leaving the bus clean) if the address phase
 * itself was NAK'd. Mirrors plugin/tests/flagship.rs's `master_write`. */
static int master_write(volatile UBYTE *base, UBYTE addr7, const UBYTE *bytes, int n)
{
    int i;

    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_ACK);
    reg_write(base, REG_A0_AREA, (UBYTE)(addr7 << 1));
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STA | CTRL_ACK);
    wait_pin_active(base);
    if (reg_read(base, REG_S1) & STATUS_LRB_AD0) {
        reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK);
        return 0;
    }
    for (i = 0; i < n; i++) {
        reg_write(base, REG_A0_AREA, bytes[i]);
        wait_pin_active(base);
    }
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK);
    return 1;
}

/* Full master-receive of `n` bytes, implementing the PCF8584 dummy-read
 * pipeline exactly as docs/board-facts.md section 4 describes it: N+1
 * total S0 reads for N data bytes, with ACK cleared immediately before
 * the read call that arms the *final* byte (call index n-1, where call
 * index 0 is the dummy read) so the device sees a NACK on its last
 * byte. Returns 0 if the address phase was NAK'd. Mirrors
 * plugin/tests/flagship.rs's `master_read_bytes` line-for-line, with
 * `wait_pin_active` standing in for its `board.tick(CCK_PER_BYTE_PHASE)`
 * (see this file's header timing note). */
static int master_read_bytes(volatile UBYTE *base, UBYTE addr7, UBYTE *out, int n)
{
    int call_index;
    UBYTE byte;

    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_ACK);
    reg_write(base, REG_A0_AREA, (UBYTE)((addr7 << 1) | 1));
    reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STA | CTRL_ACK);
    wait_pin_active(base);
    if (reg_read(base, REG_S1) & STATUS_LRB_AD0) {
        reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK);
        return 0;
    }
    if (n == 0) {
        reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK);
        return 1;
    }

    for (call_index = 0; call_index <= n; call_index++) {
        if (call_index == n - 1) {
            reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO); /* ACK=0: arms the final byte */
        }
        byte = reg_read(base, REG_A0_AREA);
        if (call_index >= 1) {
            out[call_index - 1] = byte;
        }
        if (call_index == n) {
            break;
        }
        if (call_index == n - 1) {
            wait_pin_active(base);
            reg_write(base, REG_S1, CTRL_PIN | CTRL_ESO | CTRL_STO | CTRL_ACK);
        } else {
            wait_pin_active(base);
        }
    }
    return 1;
}

/* This cross-toolchain's libgcc.a ships neither float support routines
 * nor generic 32-bit multiply/divide (confirmed by hand: linking pulls
 * in undefined references to __mulsf3 and even __mulsi3/__divsi3) --
 * so, same zero-libgcc-dependency discipline icy_probe.c already uses
 * for the rest of this probe, temperature math stays fixed-point
 * (sixteenths of a degree, the LTC2990's own native unit -- no float
 * anywhere) and the two multiply/divide operations `fan_curve` needs
 * are hand-rolled shift-and-add routines rather than relying on
 * whatever GCC would otherwise emit a libcall for. */
static LONG mul32(LONG a, LONG b)
{
    LONG result = 0;
    int neg = (a < 0) != (b < 0);
    ULONG ua = (a < 0) ? (ULONG)(-a) : (ULONG)a;
    ULONG ub = (b < 0) ? (ULONG)(-b) : (ULONG)b;
    while (ub) {
        if (ub & 1)
            result += ua;
        ua <<= 1;
        ub >>= 1;
    }
    return neg ? -result : result;
}

/* Restoring division, positive operands only -- all this probe's uses
 * are (positive numerator) / (small positive constant). */
static LONG udiv32(ULONG a, ULONG b)
{
    LONG q = 0;
    while (a >= b) {
        a -= b;
        q++;
    }
    return q;
}

/* Mirrors devices::ltc2990::encode_temp13's format (and
 * plugin/tests/flagship.rs's own `decode_temp13`): 13-bit two's
 * complement, 0.0625C/LSB -- returned directly in those same
 * sixteenths-of-a-degree units (see the fixed-point note above), not
 * converted to a whole-degree float. */
static LONG decode_temp16(UBYTE msb, UBYTE lsb)
{
    LONG raw = (((LONG)(msb & 0x1F)) << 8) | (LONG)lsb;
    if (raw & 0x1000)
        raw -= 0x2000;
    return raw;
}

static int read_ltc2990_tint16(volatile UBYTE *base, LONG *out)
{
    UBYTE reg = REG_TINT_MSB;
    UBYTE buf[2];
    if (!master_write(base, LTC2990_ADDRESS, &reg, 1))
        return 0;
    if (!master_read_bytes(base, LTC2990_ADDRESS, buf, 2))
        return 0;
    *out = decode_temp16(buf[0], buf[1]);
    return 1;
}

/* Same deliberately simple curve as flagship.rs's own `fan_curve`: off
 * below 40C, full speed above 70C, linear ramp between -- `temp16` is
 * in sixteenths of a degree (see above), so the thresholds are too
 * (40C = 640, 70C = 1120). */
static UBYTE fan_curve(LONG temp16)
{
    if (temp16 < 640)
        return 0;
    if (temp16 > 1120)
        return 255;
    return (UBYTE)udiv32((ULONG)mul32(temp16 - 640, 255), 480);
}

static int test_main(void)
{
    struct ConfigDev *cd;
    volatile UBYTE *base;
    int fails = 0;
    int i;
    LONG cool_temp16 = 0, hot_temp16 = 0;
    UBYTE duty;
    UBYTE pwmv_write[2];

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

    /* Before the scripted ramp: the scenario's cck=0 event has already
     * set Tint to 25.0C (thermal-scenario.txt) by the time any bus
     * transaction can complete, so a single read should already see it.
     * (320, 480) = (20.0C, 30.0C) in sixteenths of a degree. */
    read_ltc2990_tint16(base, &cool_temp16);
    raw_str(MSG_DIAG_COOL_TEMP16);
    raw_long(cool_temp16);
    raw_put('\n');
    report(NAME_COOL_BASELINE, cool_temp16 > 320 && cool_temp16 < 480, &fails);

    /* Poll until the scripted ramp (cck=1000, 25.0C -> 60.0C) has fired
     * -- see this file's header timing note for why polling, not a
     * fixed delay. 800 = 50.0C in sixteenths of a degree. */
    for (i = 0; i < TEMP_POLL_MAX; i++) {
        if (read_ltc2990_tint16(base, &hot_temp16) && hot_temp16 > 800)
            break;
    }
    report(NAME_TEMP_RISES, hot_temp16 > 800, &fails);

    /* Apply this probe's own fan curve to the (now hot) reading and
     * drive the real MAX31760, same as FannyCtl/simplesensors would. */
    duty = fan_curve(hot_temp16);
    pwmv_write[0] = REG_PWMV;
    pwmv_write[1] = duty;
    report(NAME_FAN_DUTY_ACK, master_write(base, MAX31760_ADDRESS, pwmv_write, 2), &fails);

    /* Poll the tach registers until the virtual fan's physical spin-up
     * ramp (plugin/src/fan.rs) shows a nonzero reading. */
    {
        UBYTE tach_ptr = REG_TC1H;
        UBYTE tach[2];
        int spun_up = 0;
        for (i = 0; i < FAN_POLL_MAX; i++) {
            if (!master_write(base, MAX31760_ADDRESS, &tach_ptr, 1))
                continue;
            if (!master_read_bytes(base, MAX31760_ADDRESS, tach, 2))
                continue;
            if (tach[0] != 0 || tach[1] != 0) {
                spun_up = 1;
                break;
            }
        }
        report(NAME_FAN_SPINS_UP, spun_up, &fails);
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
