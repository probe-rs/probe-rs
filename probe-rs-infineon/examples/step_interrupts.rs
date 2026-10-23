//! Steps one core of a TC275 instruction by instruction and records where each step ends, to
//! see whether a step can enter an interrupt handler. Prints the PC and ICR after every step and
//! counts the steps that ended inside a handler (ICR.CCPN != 0, the CPU runs at an interrupt
//! priority). The core runs again at the end.
//!
//! `--pend <SRC address>` sets the request flag of that service request node (SRC.SETR) before
//! every step, so that an interrupt is pending when the step starts; the node must be enabled and
//! routed to the stepped core. `--allow` steps with interrupts enabled (the default masks them).
//!
//! ```text
//! cargo run -p probe-rs-infineon --example step_interrupts -- <core> [steps] [--pend <src>] [--allow]
//! ```

use std::time::Duration;

use probe_rs::{
    MemoryInterface, Permissions, RegisterId,
    architecture::tricore::registers::{CSFR_ICR, CSFR_PC},
    probe::{DebugProbeSelector, list::Lister},
};

/// SRC.SETR: set the service request flag.
const SRC_SETR: u32 = 1 << 26;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: step_interrupts <core> [steps] [--pend <src>] [--allow]";
    let mut args = std::env::args().skip(1);
    let core_index: usize = args.next().ok_or(usage)?.parse()?;
    let mut steps = 50;
    let mut pend = None;
    let mut allow = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pend" => {
                let src = args.next().ok_or(usage)?;
                pend = Some(u32::from_str_radix(src.trim_start_matches("0x"), 16)?);
            }
            "--allow" => allow = true,
            n => steps = n.parse()?,
        }
    }

    probe_rs_infineon::register_plugin();
    let selector = std::env::var("PROBE_RS_PROBE").unwrap_or_else(|_| "058b:0000".to_string());
    let mut session = Lister::new()
        .open(selector.parse::<DebugProbeSelector>()?)?
        .attach("TC275", Permissions::default())?;
    session.set_tricore_mask_step_interrupts(core_index, !allow)?;
    let mut core = session.core(core_index)?;
    core.halt(Duration::from_millis(500))?;

    let start_icr: u32 = core.read_core_reg(RegisterId(CSFR_ICR))?;
    let start_pc: u32 = core.read_core_reg(RegisterId(CSFR_PC))?;
    println!(
        "CPU{core_index} halted at {start_pc:#010x}, ICR {start_icr:#010x} (IE {}), interrupts {} during steps",
        start_icr >> 15 & 1,
        if allow { "enabled" } else { "masked" }
    );

    let mut in_handler = 0;
    for i in 0..steps {
        if let Some(src) = pend {
            let value = core.read_word_32(src as u64)?;
            core.write_word_32(src as u64, value | SRC_SETR)?;
        }
        let pc = core.step()?.pc as u32;
        let icr: u32 = core.read_core_reg(RegisterId(CSFR_ICR))?;
        let ccpn = icr & 0xFF;
        if ccpn != 0 {
            in_handler += 1;
        }
        println!("step {i:3}: PC {pc:#010x}  ICR {icr:#010x}  CCPN {ccpn}");
    }
    let end_icr: u32 = core.read_core_reg(RegisterId(CSFR_ICR))?;
    println!(
        "{in_handler} of {steps} steps ended in an interrupt handler (CCPN != 0); ICR at the end {end_icr:#010x}"
    );
    core.run()?;
    Ok(())
}
