//! probe-rs flash loader for the HiSilicon WS63 (Hi3863) on-chip SFC NOR flash.
//!
//! This is the loader blob that probe-rs uploads to target RAM and calls to erase
//! and program flash. It drives the WS63 Serial Flash Controller (SFC v150) in
//! register/command mode to issue standard SPI-NOR commands (WREN/RDSR/SE/PP).
//!
//! Ground truth: `fbb_ws63` `hal_sfc_v150` (`hal_sfc_v150.c`,
//! `hal_sfc_v150_regs_def.h`) corroborated by HiSpark Studio's OpenOCD
//! `src/flash/nor/ws63.c`. **UNVALIDATED on silicon** — every register field and
//! the XIP-base mapping below is reverse-engineered and must be checked on a board
//! (see ../README.md). Built for `riscv32imc` (WS63 has no atomics; probe-rs does
//! not preserve FP across algo calls, so no F/A is used).

#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};
use flash_algorithm::*;

// Return trampoline. probe-rs sets `ra = load_address` and, per the CMSIS-Pack
// convention, expects a routine's `ret` to self-trap there. The flash-algorithm
// crate lays its functions out at `.entry` with no trap, so without this the core
// runs off into whatever links at offset 0 (EraseSector) and the routine call
// times out. link.x KEEPs `.trampoline` first in PrgCode, so this `ebreak` sits at
// load_address; Init/EraseSector/ProgramPage/UnInit all `ret` here and halt.
core::arch::global_asm!(
    ".pushsection .trampoline, \"ax\"",
    ".globl _flash_algo_return_trap",
    "_flash_algo_return_trap:",
    "ebreak",
    ".popsection",
);

// ---- SFC v150 register map (base 0x4800_0000) ----
const SFC_BASE: u32 = 0x4800_0000;
const CMD_CONFIG: u32 = SFC_BASE + 0x300; // [0]start [1]sel_cs [3]addr_en [7]data_en
// [8]rw(0=wr,1=rd) [9:14]data_cnt=len-1 [17:19]if_type
const CMD_INS: u32 = SFC_BASE + 0x308; // [7:0] opcode
const CMD_ADDR: u32 = SFC_BASE + 0x30C; // flash chip offset
const CMD_DATABUF: u32 = SFC_BASE + 0x400; // 16 x u32

/// Flash chip offset 0 is mapped to CPU address 0x200000 (SFC bus_base_addr_cs0).
const XIP_BASE: u32 = 0x0020_0000;

// Standard SPI-NOR opcodes (flash_common_config.h).
const OP_WREN: u32 = 0x06;
const OP_RDSR: u32 = 0x05;
const OP_SE_4K: u32 = 0x20; // 4 KiB sector erase
const OP_PP: u32 = 0x02; // page program
const OP_WRSR: u32 = 0x01; // write status register (used to clear block-protect)

const RW_WRITE: u32 = 0;
const RW_READ: u32 = 1;
const IF_STD: u32 = 0; // standard (1-1-1) SPI

/// cmd_databuf is 16 x 4 bytes = 64 bytes max per reg-mode data transfer.
const SFC_MAX_DATA: usize = 64;

/// WIP-poll budget (each iteration issues one RDSR).
const WIP_POLL_LIMIT: u32 = 2_000_000;

#[inline(always)]
fn wr(addr: u32, val: u32) {
    // SAFETY: fixed SFC MMIO addresses.
    unsafe { write_volatile(addr as *mut u32, val) }
}
#[inline(always)]
fn rd(addr: u32) -> u32 {
    // SAFETY: fixed SFC MMIO addresses.
    unsafe { read_volatile(addr as *const u32) }
}

/// Assemble `cmd_config` (bit layout from `hal_sfc_v150_regs_def.h`). `start` is
/// always set; `sel_cs` follows the SDK (which writes 1).
#[inline(always)]
fn cmd_config(addr_en: bool, data_en: bool, rw: u32, data_cnt: u32) -> u32 {
    1                                  // [0]    start
        | (1 << 1)                     // [1]    sel_cs (SDK writes 1)
        | ((addr_en as u32) << 3)      // [3]    addr_en
        | ((data_en as u32) << 7)      // [7]    data_en
        | ((rw & 0x1) << 8)            // [8]    rw (0=write, 1=read)
        | ((data_cnt & 0x3f) << 9)     // [9:14] data_cnt = byte_count - 1
        | ((IF_STD & 0x7) << 17) // [17:19] mem_if_type
}

/// Poll the `start` bit until the controller finishes the transaction.
///
/// Bounded so a routine can never spin forever (e.g. if the SFC `start` bit is
/// never cleared); it always returns to the trampoline and halts.
#[inline(always)]
fn wait_cmd_done() {
    let mut n: u32 = 0;
    while rd(CMD_CONFIG) & 1 != 0 {
        n = n.wrapping_add(1);
        if n > WIP_POLL_LIMIT {
            break;
        }
    }
}

/// Issue WREN (sets the flash WEL latch). Required before every erase/program.
fn write_enable() {
    wr(CMD_INS, OP_WREN);
    wr(CMD_CONFIG, cmd_config(false, false, RW_WRITE, 0));
    wait_cmd_done();
}

/// Poll RDSR until the WIP bit (status bit 0) clears.
fn wait_ready() -> Result<(), ErrorCode> {
    for _ in 0..WIP_POLL_LIMIT {
        wr(CMD_INS, OP_RDSR);
        wr(CMD_CONFIG, cmd_config(false, true, RW_READ, 0)); // read 1 status byte
        wait_cmd_done();
        if rd(CMD_DATABUF) & 0x1 == 0 {
            return Ok(());
        }
    }
    Err(ErrorCode::new(0x57630001).unwrap()) // WIP wait timeout
}

struct Ws63Algo;

algorithm!(Ws63Algo, {
    device_name: "ws63",
    device_type: DeviceType::Onchip,
    flash_address: 0x200000,
    flash_size: 0x800000,
    page_size: 0x100,
    empty_value: 0xFF,
    program_time_out: 1000,
    erase_time_out: 5000,
    sectors: [{
        size: 0x1000,
        address: 0x0,
    }]
});

impl FlashAlgorithm for Ws63Algo {
    fn new(_address: u32, _clock: u32, _function: Function) -> Result<Self, ErrorCode> {
        // The SFC is left as configured by the boot ROM / flashboot (XIP bus mode);
        // the register/command path used below operates alongside it.
        //
        // Clear the flash chip's block-protect bits (status register BP0..BP2). On
        // this board (GD25Q32) they are set at power-on (RDSR=0x1e) and the chip
        // silently rejects every erase/program until they are cleared. Hardware-
        // verified: WREN + WRSR(status=0x00) is what unblocks SE/PP.
        write_enable();
        wr(CMD_DATABUF, 0x00); // status register value = 0 (BP cleared)
        wr(CMD_INS, OP_WRSR);
        wr(CMD_CONFIG, cmd_config(false, true, RW_WRITE, 0)); // write 1 status byte
        wait_cmd_done();
        wait_ready()?;
        Ok(Self)
    }

    fn erase_sector(&mut self, address: u32) -> Result<(), ErrorCode> {
        let off = address.wrapping_sub(XIP_BASE); // CPU XIP addr -> flash offset
        write_enable();
        wr(CMD_INS, OP_SE_4K);
        wr(CMD_ADDR, off);
        wr(CMD_CONFIG, cmd_config(true, false, RW_WRITE, 0));
        wait_cmd_done();
        wait_ready()
    }

    fn program_page(&mut self, address: u32, data: &[u8]) -> Result<(), ErrorCode> {
        let mut off = address.wrapping_sub(XIP_BASE);
        for chunk in data.chunks(SFC_MAX_DATA) {
            write_enable();
            // Pack bytes into cmd_databuf words (little-endian); pad the tail word
            // with 0xFF — `data_cnt` bounds the actual byte count, so padding is
            // never programmed.
            let words = chunk.len().div_ceil(4);
            for w in 0..words {
                let mut v = 0xFFFF_FFFFu32;
                for b in 0..4 {
                    let idx = w * 4 + b;
                    if idx < chunk.len() {
                        let shift = (b * 8) as u32;
                        v = (v & !(0xFFu32 << shift)) | ((chunk[idx] as u32) << shift);
                    }
                }
                wr(CMD_DATABUF + (w as u32) * 4, v);
            }
            wr(CMD_INS, OP_PP);
            wr(CMD_ADDR, off);
            wr(
                CMD_CONFIG,
                cmd_config(true, true, RW_WRITE, (chunk.len() as u32) - 1),
            );
            wait_cmd_done();
            wait_ready()?;
            off += chunk.len() as u32;
        }
        Ok(())
    }
}
