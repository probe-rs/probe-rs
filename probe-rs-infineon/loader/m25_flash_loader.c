/*
 * m25_flash_loader.c - M25-4b: our own AURIX PFLASH flash loader.
 *
 * Runs on CPU0 from PSPR (linked at 0x70100000), driven by the host (TCF agent)
 * through a mailbox in DSPR at 0x7000F000.  Built per device:
 *   default   TC37x (TC3xx DMU)          -> m25_flash_loader.bin
 *   -DTC27X   TC27x (TC2xx FLASH0 module) -> m25_flash_loader_tc27x.bin
 *
 * Flash command sequences (same on TC2xx and TC3xx, from the Infineon iLLD
 * IfxFlash driver, Boost Software License 1.0):
 *   - command interface : 0xAF000000 | {0x5554, 0xAA50, 0xAA58, 0xAAA8, 0x55F0}
 * Device specific (TC27x values checked against the Infineon AURIX Flasher's
 * flax_TC2xx.hex algorithm and DeviceConfigs/TC27x_D_step.json):
 *   TC37x: status DMU_HF_STATUS 0xF8040010 (P0BUSY bit2, P1BUSY bit3),
 *          errors DMU_HF_ERRSR 0xF8040034, WDTS_CON0 0xF00362A8,
 *          WDTCPU0_CON0 0xF003624C, 16 KiB logical sectors
 *   TC27x: status/errors FLASH0_FSR 0xF8002010 (FABUSY..P1BUSY bits 0-4,
 *          OPER/SQER/PROER bits 11-13), WDTS_CON0 0xF00360F0,
 *          WDTCPU0_CON0 0xF0036100, logical sectors of 16 KiB..256 KiB
 *          (erase = one "erase logical sector range" command)
 *   WDT CON0: ENDINIT[0], LCK[1], PW[15:2]; PW[7:2] read back inverted, so
 *   the password is (read PW) ^ 0x3F (iLLD IfxScuWdt_get*Password).
 *
 * Mailbox protocol (32-bit words at 0x7000F000):
 *   [0] magic    host:0 -> loader:0x4D32354D ("M25M") when ready
 *   [1] cmd      0=idle 1=erase 2=program 3=exit
 *   [2] arg0     erase: sector addr (0xA0xxxxxx) ; program: page addr
 *   [3] arg1     erase: sector count               ; program: byte count (n*32)
 *   [4] arg2     program: source addr in DSPR
 *   [5] status   0=ok else DMU_ERRSR value
 *   [6] done     0=busy 1=command finished
 *   [7] progress units processed (sectors / pages)
 */

typedef unsigned int u32;

#define MB            ((volatile u32 *)0x7000F000u)
#define CMD_BASE      0xAF000000u
#ifdef TC27X
#define FLASH0_FSR    (*(volatile u32 *)0xF8002010u)
#define FSR_BUSY      0x1Fu          /* FABUSY, D0BUSY, D1BUSY, P0BUSY, P1BUSY */
#define FSR_ERRORS    0x3800u        /* OPER, SQER, PROER */
#define WDTS_CON0     (*(volatile u32 *)0xF00360F0u)
#define WDTS_CON1     (*(volatile u32 *)0xF00360F4u)
#define WDTCPU0_CON0  (*(volatile u32 *)0xF0036100u)
#define WDTCPU0_CON1  (*(volatile u32 *)0xF0036104u)
#else
#define DMU_STATUS    (*(volatile u32 *)0xF8040010u)
#define DMU_ERRSR     (*(volatile u32 *)0xF8040034u)
#define WDTS_CON0     (*(volatile u32 *)0xF00362A8u)
#define WDTS_CON1     (*(volatile u32 *)0xF00362ACu)
#define WDTCPU0_CON0  (*(volatile u32 *)0xF003624Cu)
#define WDTCPU0_CON1  (*(volatile u32 *)0xF0036250u)
#endif

#define CMD_IDLE      0u
#define CMD_ERASE     1u
#define CMD_PROGRAM   2u
#define CMD_EXIT      3u

#define MAGIC         0x4D32354Du

#define MB_MAGIC      0
#define MB_CMD        1
#define MB_ARG0       2
#define MB_ARG1       3
#define MB_ARG2       4
#define MB_STATUS     5
#define MB_DONE       6
#define MB_PROGRESS   7

#define PFLASH_SECTOR_SIZE 0x4000u   /* 16 KiB logical sector */
#define PFLASH_PAGE_SIZE   32u       /* 32 bytes per program page */

static void dsync(void)
{
    __asm__ volatile ("dsync");
}

/* ---- flash command interface (iLLD sequences) ---- */

static void flash_cmd(u32 v)
{
    *(volatile u32 *)(CMD_BASE | 0x5554u) = v;
    dsync();
}

static void flash_cmd2(u32 p1, u32 p2, u32 c1, u32 c2)
{
    *(volatile u32 *)(CMD_BASE | 0xAA50u) = p1;
    *(volatile u32 *)(CMD_BASE | 0xAA58u) = p2;
    *(volatile u32 *)(CMD_BASE | 0xAAA8u) = c1;
    *(volatile u32 *)(CMD_BASE | 0xAAA8u) = c2;
    dsync();
}

static void load_page(u32 w0, u32 w1)
{
    volatile u32 *a = (volatile u32 *)(CMD_BASE | 0x55F0u);
    a[0] = w0;
    dsync();               /* purge store buffers: avoid 64-bit merging */
    a[1] = w1;
    dsync();
}

static void wait_unbusy(void)
{
#ifdef TC27X
    /* wait until the flash array and all banks are idle */
    while (FLASH0_FSR & FSR_BUSY) { }
#else
    /* wait until both program flash banks are idle */
    while (DMU_STATUS & ((1u << 2) | (1u << 3))) { }
#endif
    dsync();
}

/* error flags of the last command (0 = ok) */
static u32 flash_errors(void)
{
#ifdef TC27X
    return FLASH0_FSR & FSR_ERRORS;
#else
    return DMU_ERRSR;
#endif
}

/* the password of a WDT: PW[7:2] reads back inverted */
static u32 wdt_password(volatile u32 *con0_reg)
{
    return ((*con0_reg >> 2) & 0x3FFFu) ^ 0x003Fu;
}

/* ---- watchdog handling (standard flasher practice) ---- */

/* clear ENDINIT of a WDT CON0; bounded wait (the password field may read back
 * hidden/zero when the application configured it - never hang) */
static int wdt_clear_endinit(volatile u32 *con0_reg)
{
    u32 con0 = *con0_reg;
    u32 pw   = wdt_password(con0_reg);
    u32 rel  = con0 & 0xFFFF0000u;
    u32 i;
    if (con0 & (1u << 1))                       /* LCK set: password access pattern */
        *con0_reg = (1u << 0) | (0u << 1) | (pw << 2) | rel;
    *con0_reg = (0u << 0) | (1u << 1) | (pw << 2) | rel;   /* ENDINIT=0, LCK=1 */
    dsync();
    for (i = 0u; i < 200000u; i++) {
        if ((*con0_reg & 1u) == 0u) {
            return 1;                           /* ENDINIT cleared */
        }
    }
    return 0;                                   /* could not clear (password hidden) */
}

/* set ENDINIT again.  CON0 is locked (LCK=1) after the modify access that
 * cleared ENDINIT, so - like the clear - a password access (LCK=0) must come
 * first; a single modify write is ignored and left ENDINIT cleared. */
static void wdt_set_endinit(volatile u32 *con0_reg, u32 pw)
{
    u32 rel = *con0_reg & 0xFFFF0000u;
    u32 i;
    if (*con0_reg & (1u << 1))                               /* locked: password access */
        *con0_reg = (1u << 0) | (0u << 1) | (pw << 2) | rel;
    *con0_reg = (1u << 0) | (1u << 1) | (pw << 2) | rel;     /* modify: ENDINIT=1, LCK=1 */
    dsync();
    for (i = 0u; i < 200000u; i++) {
        if (*con0_reg & 1u) {
            break;
        }
    }
}

/* disable the CPU0 watchdog and the safety watchdog (like professional flashers) */
static void wdt_disable(void)
{
    u32 pw = wdt_password(&WDTCPU0_CON0);
    if (wdt_clear_endinit(&WDTCPU0_CON0)) {
        WDTCPU0_CON1 = WDTCPU0_CON1 | (1u << 3);   /* DR = 1 (disable request) */
        wdt_set_endinit(&WDTCPU0_CON0, pw);
    }
    pw = wdt_password(&WDTS_CON0);
    if (wdt_clear_endinit(&WDTS_CON0)) {
        WDTS_CON1 = WDTS_CON1 | (1u << 3);         /* DR = 1 */
        wdt_set_endinit(&WDTS_CON0, pw);
    }
}

/* ---- safety ENDINIT handling (SCU WDTS) ---- */

static void clear_endinit(void)
{
    wdt_clear_endinit(&WDTS_CON0);
}

static void set_endinit(void)
{
    wdt_set_endinit(&WDTS_CON0, wdt_password(&WDTS_CON0));
}

/* ---- operations ---- */

/* erase 'count' consecutive logical sectors starting at sector_addr */
static u32 erase_sectors(u32 sector_addr, u32 count)
{
    u32 err;
    flash_cmd(0xFA);                                  /* clear status */
    flash_cmd2(sector_addr, count, 0x80u, 0x50u);     /* erase logical sector range */
    wait_unbusy();
    err = flash_errors();
    flash_cmd(0xF0);                                  /* reset to read mode */
    return err;
}

static u32 program_one_page(u32 page_addr, const u32 *src)
{
    u32 err;
    flash_cmd(0xFA);                                  /* clear status */
    if ((page_addr & 0xFF000000u) == 0xA0000000u)
        flash_cmd(0x50);                              /* enter page mode (PFLASH) */
    else
        flash_cmd(0x5D);                              /* enter page mode (DFLASH) */
    wait_unbusy();                                    /* iLLD: wait after enterPageMode */
    load_page(src[0], src[1]);
    load_page(src[2], src[3]);
    load_page(src[4], src[5]);
    load_page(src[6], src[7]);
    flash_cmd2(page_addr, 0u, 0xA0u, 0xAAu);          /* write page */
    wait_unbusy();
    err = flash_errors();
    flash_cmd(0xF0);                                  /* reset to read mode */
    return err;
}

/* ---- entry point ---- */

/* placed first in PSPR by the linker script: the host starts at 0x70100000 */
void _start(void) __attribute__((section(".text.entry")));

void _start(void)
{
    MB[MB_STATUS]   = 0u;
    MB[MB_DONE]     = 0u;
    MB[MB_PROGRESS] = 0u;
    MB[MB_CMD]      = 0u;
    MB[MB_MAGIC]    = MAGIC;
    wdt_disable();   /* best effort: bounded waits, cannot hang */
    /* NOTE: if the application configured a hidden WDT password, the disable is
     * a no-op; the host can reset the device first (UCB default password). */

    for (;;) {
        u32 cmd = MB[MB_CMD];
        u32 st  = 0u;

        if (cmd == CMD_IDLE) {
            continue;
        }

        clear_endinit();

        if (cmd == CMD_ERASE) {
            u32 addr = MB[MB_ARG0];
            u32 n    = MB[MB_ARG1];
#ifdef TC27X
            /* logical sectors differ in size: the hardware walks the range */
            st = erase_sectors(addr, n);
            MB[MB_PROGRESS] = n;
#else
            u32 i;
            for (i = 0u; i < n; i++) {
                st = erase_sectors(addr + i * PFLASH_SECTOR_SIZE, 1u);
                MB[MB_PROGRESS] = i + 1u;
                if (st != 0u) {
                    break;
                }
            }
#endif
        }
        else if (cmd == CMD_PROGRAM) {
            u32 addr  = MB[MB_ARG0];
            u32 size  = MB[MB_ARG1];
            u32 src   = MB[MB_ARG2];
            u32 pages = (size + PFLASH_PAGE_SIZE - 1u) / PFLASH_PAGE_SIZE;
            u32 i;
            for (i = 0u; i < pages; i++) {
                st = program_one_page(addr + i * PFLASH_PAGE_SIZE,
                                      (const u32 *)(src + i * PFLASH_PAGE_SIZE));
                MB[MB_PROGRESS] = i + 1u;
                if (st != 0u) {
                    break;
                }
            }
        }
        else if (cmd == CMD_EXIT) {
            set_endinit();
            MB[MB_STATUS] = 0u;
            MB[MB_DONE]   = 1u;
            MB[MB_CMD]    = 0u;
            for (;;) { }                                  /* idle forever */
        }
        else {
            st = 0xFFFFFFFFu;                             /* unknown command */
        }

        set_endinit();
        MB[MB_STATUS] = st;
        MB[MB_DONE]   = 1u;
        MB[MB_CMD]    = 0u;
    }
}
