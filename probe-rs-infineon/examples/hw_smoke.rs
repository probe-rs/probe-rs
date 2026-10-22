//! Hardware smoke test of TriCore debugging through a TAS server (PRD tests T-02 to T-09).
//!
//! The probe's bus is also asked for map 1 (T-03), which this adapter blocks.
//!
//! It needs the demo image `demo_flash.elf` in PFLASH (PF1, 0x8021C000) of a TC275. The
//! application's context on CPU0 is abandoned to run the demo, so the device is reset at the
//! end. Nothing is written to flash.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example hw_smoke -- path/to/demo_flash.elf
//! ```
//!
//! `PROBE_RS_PROBE` selects another probe, for example the MCD probe (PRD test T-14):
//!
//! ```text
//! set PROBE_RS_PROBE=058b:0001:C:\path\to\mcd_tas.dll
//! cargo run -p probe-rs-infineon --features mcd --example hw_smoke
//! ```

use std::time::Duration;

use probe_rs::{
    BreakpointCause, Core, CoreStatus, HaltReason, InstructionSet, MemoryInterface, Permissions,
    RegisterId,
    architecture::tricore::{
        bus::{AddressMap, TriCoreError},
        registers::{CSFR_A0, CSFR_D0, CSFR_ICR, CSFR_PC, CSFR_PCXI},
    },
    probe::{DebugProbeSelector, list::Lister},
};
use probe_rs_debug::{DebugInfo, DebugRegisters, exception_handler_for_core};

const STACK_TOP: u32 = 0x7000_C000;
const SCRATCH: u64 = 0x7000_A000;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Report {
    failed: usize,
}

impl Report {
    fn check(&mut self, id: &str, what: &str, ok: bool, detail: String) {
        println!(
            "[{}] {id} {what}: {detail}",
            if ok { "PASS" } else { "FAIL" }
        );
        if !ok {
            self.failed += 1;
        }
    }
}

/// The PT_LOAD segments of an ELF32 little-endian file: (address, data).
fn load_segments(elf: &[u8]) -> Result<Vec<(u32, Vec<u8>)>> {
    if elf.get(..4) != Some(b"\x7fELF".as_slice()) || elf[4] != 1 || elf[5] != 1 {
        return Err("not an ELF32 little-endian file".into());
    }
    let u16_at = |o: usize| u16::from_le_bytes([elf[o], elf[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(elf[o..o + 4].try_into().unwrap());
    let (phoff, phent, phnum) = (u32_at(28) as usize, u16_at(42), u16_at(44));
    let mut segments = Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * phent;
        if u32_at(ph) != 1 {
            continue;
        }
        let (offset, vaddr, filesz) = (u32_at(ph + 4) as usize, u32_at(ph + 8), u32_at(ph + 16));
        segments.push((vaddr, elf[offset..offset + filesz as usize].to_vec()));
    }
    Ok(segments)
}

/// The value of symbol `name` in the symbol table of an ELF32 little-endian file.
fn symbol_address(elf: &[u8], name: &str) -> Option<u64> {
    let u16_at = |o: usize| u16::from_le_bytes([elf[o], elf[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(elf[o..o + 4].try_into().unwrap()) as usize;
    let (shoff, shent, shnum) = (u32_at(32), u16_at(46), u16_at(48));
    let section = |i: usize| shoff + i * shent;
    let symtab = (0..shnum).map(section).find(|&sh| u32_at(sh + 4) == 2)?;
    let strtab = section(u32_at(symtab + 24));
    let (strings, symbols, size) = (
        u32_at(strtab + 16),
        u32_at(symtab + 16),
        u32_at(symtab + 20),
    );
    (symbols..symbols + size).step_by(16).find_map(|sym| {
        let start = strings + u32_at(sym);
        let end = start + elf[start..].iter().position(|&b| b == 0)?;
        (&elf[start..end] == name.as_bytes()).then(|| u32_at(sym + 4) as u64)
    })
}

fn reg(core: &mut Core, id: u16) -> Result<u32> {
    Ok(core.read_core_reg(RegisterId(id))?)
}

fn main() -> Result<()> {
    let elf_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\work\aurix_tcf_demo\build\demo_flash.elf".to_string());
    let elf = std::fs::read(&elf_path)?;
    let entry = u32::from_le_bytes(elf[24..28].try_into()?);
    let debug_info = DebugInfo::from_file(&elf_path)?;
    let square = symbol_address(&elf, "square").ok_or("no symbol square")?;

    probe_rs_infineon::register_plugin();
    let selector: DebugProbeSelector = std::env::var("PROBE_RS_PROBE")
        .unwrap_or_else(|_| "058b:0000".to_string())
        .parse()?;
    println!("probe {selector}");
    let probe = Lister::new().open(selector)?;
    println!("{}", probe.get_name());
    let mut session = probe.attach("TC275", Permissions::default())?;
    let mut report = Report { failed: 0 };

    // T-03: map 1 (EEC / MCDS) is blocked on this adapter; the read must fail, not return 0.
    let eec = session
        .get_tricore_bus()?
        .read_u32(AddressMap::Eec, 0xF00E_0080);
    report.check(
        "T-03",
        "map 1 read refused",
        matches!(eec, Err(TriCoreError::TransferFailed { .. })),
        format!("{eec:?}"),
    );

    // T-04: halt every core.
    for id in 0..3 {
        let mut core = session.core(id)?;
        let info = core.halt(Duration::from_millis(200))?;
        let halted = core.core_halted()?;
        report.check(
            "T-04",
            &format!("halt CPU{id}"),
            halted,
            format!("PC {:#010x}", info.pc),
        );
    }

    let mut core = session.core(0)?;

    // T-07: register write and read back, then restore.
    let saved = (0..16)
        .map(|n| reg(&mut core, CSFR_D0 + 4 * n))
        .collect::<Result<Vec<_>>>()?;
    for n in 0..16u16 {
        core.write_core_reg(RegisterId(CSFR_D0 + 4 * n), 0xD000_0000 | n as u32)?;
    }
    let back = (0..16)
        .map(|n| reg(&mut core, CSFR_D0 + 4 * n))
        .collect::<Result<Vec<_>>>()?;
    for (n, value) in saved.iter().enumerate() {
        core.write_core_reg(RegisterId(CSFR_D0 + 4 * n as u16), *value)?;
    }
    let restored = (0..16)
        .map(|n| reg(&mut core, CSFR_D0 + 4 * n))
        .collect::<Result<Vec<_>>>()?;
    report.check(
        "T-07",
        "D0-D15 write/read/restore",
        back.iter()
            .enumerate()
            .all(|(n, v)| *v == 0xD000_0000 | n as u32)
            && restored == saved,
        format!(
            "D15 {:#010x} -> {:#010x} -> {:#010x}",
            saved[15], back[15], restored[15]
        ),
    );

    // T-02: memory, an unaligned range across block boundaries, then restore.
    let mut original = vec![0u8; 4096];
    core.read(SCRATCH, &mut original)?;
    let pattern = (0..2500).map(|i| (i * 13 + 7) as u8).collect::<Vec<_>>();
    core.write(SCRATCH + 3, &pattern)?;
    let mut back = vec![0u8; pattern.len()];
    core.read(SCRATCH + 3, &mut back)?;
    // The local DSPR address of CPU0 reaches the same memory.
    let global = core.read_word_32(SCRATCH + 8)?;
    let local = core.read_word_32(0xD000_A008)?;
    report.check(
        "T-02",
        "local DSPR alias",
        global == local && global != 0,
        format!("{global:#010x} == {local:#010x}"),
    );
    core.write(SCRATCH, &original)?;
    let mut check = vec![0u8; 4096];
    core.read(SCRATCH, &mut check)?;
    report.check(
        "T-02",
        "DSPR0 write/read/restore (2500 B at +3)",
        back == pattern && check == original,
        format!("{} B verified", pattern.len()),
    );
    // The demo must be in flash.
    for (address, data) in load_segments(&elf)? {
        let mut flash = vec![0u8; data.len()];
        core.read(address as u64, &mut flash)?;
        if flash != data {
            return Err(format!("flash at {address:#010x} does not hold {elf_path}").into());
        }
    }
    println!("demo found in flash, entry {entry:#010x}");

    // Start the demo on CPU0: PC = main, a fresh stack, no calling context, interrupts off.
    core.write_core_reg(RegisterId(CSFR_PC), entry)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 10), STACK_TOP)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 11), 0u32)?;
    core.write_core_reg(RegisterId(CSFR_PCXI), 0u32)?;
    let icr = reg(&mut core, CSFR_ICR)?;
    core.write_core_reg(RegisterId(CSFR_ICR), icr & !(1 << 15))?;

    // T-05: single steps move the PC.
    let mut pcs = vec![core.read_core_reg::<u32>(RegisterId(CSFR_PC))? as u64];
    for _ in 0..20 {
        pcs.push(core.step()?.pc);
    }
    let moved = pcs.windows(2).all(|w| w[0] != w[1]);
    let status = core.status()?;
    report.check(
        "T-05",
        "20 single steps",
        moved && status == CoreStatus::Halted(HaltReason::Step),
        format!("{:#x?} ... {:#010x}", &pcs[..4], pcs[20]),
    );

    // T-06: eight breakpoint units, the ninth is refused.
    for n in 0..8u64 {
        core.set_hw_breakpoint(0x8000_0000 + 0x100 * n)?;
    }
    let ninth = core.set_hw_breakpoint(0x8000_0900);
    report.check(
        "T-06",
        "9th breakpoint refused",
        ninth.is_err(),
        format!("{ninth:?}"),
    );
    core.clear_all_hw_breakpoints()?;

    core.set_hw_breakpoint(square)?;
    core.run()?;
    core.wait_for_core_halted(Duration::from_secs(2))?;
    let pc = core.read_core_reg::<u32>(RegisterId(CSFR_PC))? as u64;
    let status = core.status()?;
    report.check(
        "T-06",
        "breakpoint at square",
        pc == square
            && status == CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware)),
        format!("PC {pc:#010x}, {status:?}"),
    );

    // T-09: call stack square <- scale <- accumulate <- compute <- main.
    let registers = DebugRegisters::from_core(&mut core);
    let handler = exception_handler_for_core(core.core_type());
    let frames = debug_info.unwind(
        &mut core,
        registers,
        handler.as_ref(),
        Some(InstructionSet::TriCore),
        20,
    )?;
    let names = frames
        .iter()
        .map(|f| f.function_name.clone())
        .collect::<Vec<_>>();
    report.check(
        "T-09",
        "call stack",
        names.len() >= 5 && names[..5] == ["square", "scale", "accumulate", "compute", "main"],
        names.join(" <- "),
    );

    // Running from a breakpoint steps over it first, then the next call hits it again.
    core.run()?;
    core.wait_for_core_halted(Duration::from_secs(2))?;
    let pc = core.read_core_reg::<u32>(RegisterId(CSFR_PC))? as u64;
    report.check(
        "T-06",
        "breakpoint hit again",
        pc == square,
        format!("PC {pc:#010x}"),
    );
    core.clear_all_hw_breakpoints()?;

    // T-04: run and halt again.
    core.run()?;
    let running = core.status()? == CoreStatus::Running;
    core.halt(Duration::from_millis(200))?;
    let halted = core.status()? == CoreStatus::Halted(HaltReason::Request);
    report.check(
        "T-04",
        "run, then halt CPU0",
        running && halted,
        String::new(),
    );

    // T-08: reset and halt, then reset and run the application again.
    let info = core.reset_and_halt(Duration::from_millis(500))?;
    let halted = core.core_halted()?;
    report.check(
        "T-08",
        "reset and halt",
        halted,
        format!("PC {:#010x}", info.pc),
    );
    core.reset()?;
    std::thread::sleep(Duration::from_millis(300));
    let running = core.status()? == CoreStatus::Running;
    report.check("T-08", "reset and run", running, String::new());
    drop(core);
    drop(session);

    println!("{} check(s) failed", report.failed);
    if report.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
