//! The hardware checks that `hw_smoke` does only in part (PRD tests T-04, T-05 and T-07):
//!
//! - T-04: halting one core leaves the others as they are. While CPU0 is halted, CPU1 and
//!   CPU2 keep running (their PCs move); halting and running CPU1 leaves CPU0 halted and CPU2
//!   running.
//! - T-05: 100 single steps of the demo from `main`, each checked against the disassembly of
//!   `tricore-elf-objdump`: every PC is the start of an instruction, and it is the next
//!   instruction, the branch target, the called function or the return address of the matching
//!   call.
//! - T-07: A0-A15, PC, PSW, PCXI and ICR written, read back and restored.
//!
//! It needs the demo image `demo_flash.elf` in PFLASH (PF1, 0x8021C000). CPU0's context is
//! abandoned to run the demo, so the device is reset at the end. Nothing is written to flash.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example hw_extra -- [demo_flash.elf] [tricore-elf-objdump]
//! ```

use std::{collections::HashMap, process::Command, time::Duration};

use probe_rs::{
    Core, CoreStatus, HaltReason, Permissions, RegisterId,
    architecture::tricore::{
        bus::AddressMap,
        ocds,
        registers::{CSFR_A0, CSFR_ICR, CSFR_PC, CSFR_PCXI, CSFR_PSW},
    },
    probe::{DebugProbeSelector, list::Lister},
};

const STACK_TOP: u32 = 0x7000_C000;
const CSFR_BASE: u32 = 0xF881_0000;
const CSFR_STRIDE: u32 = 0x2_0000;
const OBJDUMP: &str = r"C:\Infineon\AURIX-Configuration-Studio-1.0.24\eclipse\tricore-gcc11\bin\tricore-elf-objdump.exe";

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

/// One instruction of the disassembly.
struct Instruction {
    len: u64,
    mnemonic: String,
    /// The address an operand names (branch or call target), if any.
    target: Option<u64>,
    text: String,
}

/// Parses `objdump -d` lines: address, bytes and instruction, separated by tabs, such as
/// `8021c020: 6d ff f0 ff call 8021c000 <square>`.
fn disassemble(objdump: &str, elf: &str) -> Result<HashMap<u64, Instruction>> {
    let output = Command::new(objdump).args(["-d", elf]).output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(address), Some(bytes), Some(rest)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(address.trim().trim_end_matches(':'), 16) else {
            continue;
        };
        let len = bytes.split_whitespace().count() as u64;
        let mut words = rest.split_whitespace();
        let mnemonic = words.next().unwrap_or("").to_string();
        let target = words.flat_map(|w| w.split(',')).find_map(|w| {
            u64::from_str_radix(w, 16)
                .ok()
                .filter(|a| *a >= 0x8000_0000)
        });
        map.insert(
            address,
            Instruction {
                len,
                mnemonic,
                target,
                text: rest.trim().to_string(),
            },
        );
    }
    Ok(map)
}

fn csfr(core: u32, offset: u32) -> u32 {
    CSFR_BASE + core * CSFR_STRIDE + offset
}

/// The DBGSR and PC of `core`, read from the bus without touching the core.
fn peek(session: &mut probe_rs::Session, core: u32) -> Result<(bool, u32)> {
    let bus = session.get_tricore_bus()?;
    let dbgsr = bus.read_u32(AddressMap::Target, csfr(core, ocds::DBGSR as u32))?;
    let pc = bus.read_u32(AddressMap::Target, csfr(core, 0xFE08))?;
    Ok((ocds::is_halted(dbgsr), pc))
}

/// Whether `core` runs: not halted, and its PC moves over a few samples.
fn runs(session: &mut probe_rs::Session, core: u32) -> Result<(bool, String)> {
    let mut pcs = Vec::new();
    let mut halted = false;
    for _ in 0..5 {
        let (h, pc) = peek(session, core)?;
        halted |= h;
        pcs.push(pc);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut distinct = pcs.clone();
    distinct.sort_unstable();
    distinct.dedup();
    Ok((
        !halted && distinct.len() > 1,
        format!(
            "CPU{core} {} PCs {:x?}",
            if halted { "halted" } else { "not halted" },
            pcs
        ),
    ))
}

fn reg(core: &mut Core, id: u16) -> Result<u32> {
    Ok(core.read_core_reg(RegisterId(id))?)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let elf = args
        .next()
        .unwrap_or_else(|| r"C:\work\aurix_tcf_demo\build\demo_flash.elf".to_string());
    let objdump = args.next().unwrap_or_else(|| OBJDUMP.to_string());
    let program = disassemble(&objdump, &elf)?;
    if program.is_empty() {
        return Err(format!("no disassembly from {objdump}").into());
    }
    let bytes = std::fs::read(&elf)?;
    let entry = u32::from_le_bytes(bytes[24..28].try_into()?);

    probe_rs_infineon::register_plugin();
    let selector: DebugProbeSelector = std::env::var("PROBE_RS_PROBE")
        .unwrap_or_else(|_| "058b:0000".to_string())
        .parse()?;
    let mut session = Lister::new()
        .open(selector)?
        .attach("TC275", Permissions::default())?;
    let mut report = Report { failed: 0 };

    // T-04: one core halts, the others run on.
    for id in 0..3 {
        let (ok, detail) = runs(&mut session, id)?;
        if !ok {
            return Err(format!("the cores must run at the start: {detail}").into());
        }
    }
    session.core(0)?.halt(Duration::from_millis(200))?;
    let (cpu0_halted, _) = peek(&mut session, 0)?;
    let (cpu1, d1) = runs(&mut session, 1)?;
    let (cpu2, d2) = runs(&mut session, 2)?;
    report.check(
        "T-04",
        "CPU0 halted, CPU1 and CPU2 run on",
        cpu0_halted && cpu1 && cpu2,
        format!("{d1}; {d2}"),
    );
    session.core(1)?.halt(Duration::from_millis(200))?;
    let (cpu0_halted, _) = peek(&mut session, 0)?;
    let (cpu1_halted, _) = peek(&mut session, 1)?;
    let (cpu2, d2) = runs(&mut session, 2)?;
    report.check(
        "T-04",
        "CPU1 halted too: CPU0 stays halted, CPU2 runs on",
        cpu0_halted && cpu1_halted && cpu2,
        d2,
    );
    session.core(1)?.run()?;
    let (cpu0_halted, _) = peek(&mut session, 0)?;
    let (cpu1, d1) = runs(&mut session, 1)?;
    let (cpu2, d2) = runs(&mut session, 2)?;
    report.check(
        "T-04",
        "CPU1 run: CPU0 stays halted, CPU1 and CPU2 run",
        cpu0_halted && cpu1 && cpu2,
        format!("{d1}; {d2}"),
    );

    let mut core = session.core(0)?;

    // T-07: A0-A15, PC, PSW, PCXI, ICR.
    let ids = (0..16u16)
        .map(|n| (format!("A{n}"), CSFR_A0 + 4 * n))
        .chain([
            ("PC".to_string(), CSFR_PC),
            ("PSW".to_string(), CSFR_PSW),
            ("PCXI".to_string(), CSFR_PCXI),
            ("ICR".to_string(), CSFR_ICR),
        ])
        .collect::<Vec<_>>();
    let saved = ids
        .iter()
        .map(|(_, id)| reg(&mut core, *id))
        .collect::<Result<Vec<_>>>()?;
    // Values that differ from the saved ones; the PC stays halfword aligned, the PSW and ICR
    // change only bits that do nothing while the core is halted (CDC count, CCPN).
    let test = |name: &str, n: usize, saved: u32| match name {
        "PC" => 0x8021_C0AA,
        "PSW" => saved ^ 0x0000_0001,
        "ICR" => saved ^ 0x0000_0001,
        "PCXI" => saved ^ 0x0000_0001,
        _ => 0xA500_0000 | (n as u32) << 4,
    };
    for (n, ((name, id), value)) in ids.iter().zip(&saved).enumerate() {
        core.write_core_reg(RegisterId(*id), test(name, n, *value))?;
    }
    let back = ids
        .iter()
        .map(|(_, id)| reg(&mut core, *id))
        .collect::<Result<Vec<_>>>()?;
    for ((_, id), value) in ids.iter().zip(&saved) {
        core.write_core_reg(RegisterId(*id), *value)?;
    }
    let restored = ids
        .iter()
        .map(|(_, id)| reg(&mut core, *id))
        .collect::<Result<Vec<_>>>()?;
    let wrong = ids
        .iter()
        .enumerate()
        .filter(|(n, (name, _))| back[*n] != test(name, *n, saved[*n]))
        .map(|(n, (name, _))| format!("{name} {:#010x}", back[n]))
        .collect::<Vec<_>>();
    report.check(
        "T-07",
        "A0-A15, PC, PSW, PCXI, ICR write/read/restore",
        wrong.is_empty() && restored == saved,
        if wrong.is_empty() {
            format!(
                "{} registers; A10 {:#010x} -> {:#010x} -> {:#010x}",
                ids.len(),
                saved[10],
                back[10],
                restored[10]
            )
        } else {
            format!("read back wrong: {}", wrong.join(", "))
        },
    );

    // T-05: start the demo on CPU0 at main (fresh stack, no calling context, interrupts off).
    core.write_core_reg(RegisterId(CSFR_PC), entry)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 10), STACK_TOP)?;
    core.write_core_reg(RegisterId(CSFR_A0 + 4 * 11), 0u32)?;
    core.write_core_reg(RegisterId(CSFR_PCXI), 0u32)?;
    let icr = reg(&mut core, CSFR_ICR)?;
    core.write_core_reg(RegisterId(CSFR_ICR), icr & !(1 << 15))?;

    let mut pc = entry as u64;
    let mut returns = Vec::new();
    let mut problems = Vec::new();
    let mut functions = Vec::new();
    for step in 0..100 {
        let Some(instruction) = program.get(&pc) else {
            problems.push(format!("step {step}: {pc:#010x} is not an instruction"));
            break;
        };
        let next = core.step()?.pc;
        let status = core.status()?;
        if status != CoreStatus::Halted(HaltReason::Step) {
            problems.push(format!("step {step}: status {status:?}"));
        }
        let fallthrough = pc + instruction.len;
        let mnemonic = instruction.mnemonic.as_str();
        let expected = if mnemonic.starts_with("call") || mnemonic.starts_with("fcall") {
            returns.push(fallthrough);
            instruction.target.into_iter().collect::<Vec<_>>()
        } else if mnemonic == "ret" || mnemonic == "fret" {
            returns.pop().into_iter().collect()
        } else if mnemonic.starts_with('j') || mnemonic.starts_with("loop") {
            [Some(fallthrough), instruction.target]
                .into_iter()
                .flatten()
                .collect()
        } else {
            vec![fallthrough]
        };
        if !expected.is_empty() && !expected.contains(&next) {
            problems.push(format!(
                "step {step}: {pc:#010x} {} went to {next:#010x}, expected {expected:x?}",
                instruction.text
            ));
        }
        if mnemonic.starts_with("call") {
            functions.push(format!("{next:#x}"));
        }
        pc = next;
    }
    report.check(
        "T-05",
        "100 single steps against the disassembly",
        problems.is_empty(),
        if problems.is_empty() {
            format!(
                "every PC an instruction start and the expected successor; {} calls, {} returns pending, ends at {pc:#010x}",
                functions.len(),
                returns.len()
            )
        } else {
            problems.join("; ")
        },
    );

    // CPU0's context is gone: reset, so the application runs again on all cores.
    core.reset()?;
    std::thread::sleep(Duration::from_millis(300));
    let running = core.status()? == CoreStatus::Running;
    report.check("reset", "application runs again", running, String::new());
    drop(core);
    drop(session);

    println!("{} check(s) failed", report.failed);
    if report.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
