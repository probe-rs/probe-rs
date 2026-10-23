//! Points CPU0 of a TC275 at the entry of an image in PFLASH or RAM, without a reset.
//!
//! For images the boot code does not start, like the demo in the PF1 test sector: it checks
//! that the flash holds the image (segments in RAM, like the demo in the CPU0 PSPR, are written
//! instead), halts CPU0 and sets PC = entry, A10 = the stack top, A11 = PCXI = 0 (the entry is
//! the outermost frame) and clears ICR.IE. CPU0 stays halted, so a debugger can attach (DAP
//! `attach` request) and set breakpoints. The application's context on CPU0 is abandoned:
//! reset the device afterwards.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example start_in_flash -- <elf> [stack top, default 0x7000C000]
//! ```

use std::time::Duration;

use probe_rs::{
    MemoryInterface, Permissions, RegisterId,
    architecture::tricore::registers::{CSFR_A0, CSFR_ICR, CSFR_PC, CSFR_PCXI},
    probe::list::Lister,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: start_in_flash <elf> [stack top]")?;
    let stack = match args.next() {
        Some(s) => u32::from_str_radix(s.trim_start_matches("0x"), 16)?,
        None => 0x7000_C000,
    };

    let elf = std::fs::read(&path)?;
    if elf.get(..4) != Some(b"\x7fELF".as_slice()) || elf[4] != 1 || elf[5] != 1 {
        return Err("not an ELF32 little-endian file".into());
    }
    let u16_at = |o: usize| u16::from_le_bytes([elf[o], elf[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(elf[o..o + 4].try_into().unwrap());
    let entry = u32_at(24);

    probe_rs_infineon::register_plugin();
    // `PROBE_RS_PROBE` selects another probe, for example the MCD probe.
    let selector = std::env::var("PROBE_RS_PROBE").unwrap_or_else(|_| "058b:0000".to_string());
    let probe = Lister::new().open(selector.parse::<probe_rs::probe::DebugProbeSelector>()?)?;
    let mut session = probe.attach("TC275", Permissions::default())?;
    let mut core = session.core(0)?;
    core.halt(Duration::from_millis(500))?;

    // A stale image would run garbage.
    let (phoff, phent, phnum) = (u32_at(28) as usize, u16_at(42), u16_at(44));
    for i in 0..phnum {
        let ph = phoff + i * phent;
        if u32_at(ph) != 1 {
            continue;
        }
        let (offset, address, size, mem_size) = (
            u32_at(ph + 4) as usize,
            u32_at(ph + 8),
            u32_at(ph + 16) as usize,
            u32_at(ph + 20) as usize,
        );
        // PFLASH is 0x8... (cached) and 0xA...; the rest is RAM (scratchpads, LMU).
        if !matches!(address >> 28, 0x8 | 0xA) {
            let mut data = elf[offset..offset + size].to_vec();
            data.resize(mem_size, 0);
            core.write(address as u64, &data)?;
            println!("RAM @{address:#010x} {mem_size} B: written");
            continue;
        }
        let mut flash = vec![0; size];
        core.read(address as u64, &mut flash)?;
        if flash != elf[offset..offset + size] {
            return Err(
                format!("flash at {address:#010x} does not hold {path}: flash it first").into(),
            );
        }
        println!("flash @{address:#010x} {size} B: matches");
    }

    core.write_core_reg(RegisterId(CSFR_PC), entry)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 10), stack)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 11), 0u32)?;
    core.write_core_reg(RegisterId(CSFR_PCXI), 0u32)?;
    let icr: u32 = core.read_core_reg(RegisterId(CSFR_ICR))?;
    core.write_core_reg(RegisterId(CSFR_ICR), icr & !(1 << 15))?;
    println!("CPU0 halted at {entry:#010x}, A10 = {stack:#010x}, A11 = PCXI = 0, ICR.IE off");
    Ok(())
}
