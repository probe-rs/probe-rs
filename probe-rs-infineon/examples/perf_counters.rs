//! Reads the performance counters of a TC275 core while it runs (CCTRL, CCNT, ICNT,
//! M1CNT-M3CNT), without halting it.
//!
//! `--window <start> <stop>` first sets a performance counter window (A-to-B): the counters are
//! cleared and count only from an execution of `start` to one of `stop`. `--word <address>`
//! also prints a 32-bit word, for example a loop counter of the program. `--run` runs a halted
//! core first. The window is cleared when the example ends.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example perf_counters -- <core> [--window <start> <stop>]
//!     [--word <address>] [--run] [--samples 5] [--interval-ms 500]
//! ```

use std::time::Duration;

use probe_rs::{
    MemoryInterface, Permissions,
    architecture::tricore::registers::CSFR_CCTRL,
    probe::{DebugProbeSelector, list::Lister},
};

fn hex(s: &str) -> Result<u64, std::num::ParseIntError> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: perf_counters <core> [--window <start> <stop>] [--word <address>] \
                 [--samples n] [--interval-ms ms]";
    let mut args = std::env::args().skip(1);
    let core_index: usize = args.next().ok_or(usage)?.parse()?;
    let (mut window, mut word, mut samples, mut interval, mut run) = (None, None, 5, 500, false);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(usage);
        match arg.as_str() {
            "--window" => window = Some((hex(&value()?)?, hex(&value()?)?)),
            "--word" => word = Some(hex(&value()?)?),
            "--run" => run = true,
            "--samples" => samples = value()?.parse()?,
            "--interval-ms" => interval = value()?.parse()?,
            _ => return Err(usage.into()),
        }
    }

    probe_rs_infineon::register_plugin();
    let selector = std::env::var("PROBE_RS_PROBE").unwrap_or_else(|_| "058b:0000".to_string());
    let mut session = Lister::new()
        .open(selector.parse::<DebugProbeSelector>()?)?
        .attach("TC275", Permissions::default())?;
    // The CSFR window of the core: CPU0 at 0xF881_0000, then every 0x2_0000.
    let cctrl = 0xF881_0000 + 0x2_0000 * core_index as u64 + CSFR_CCTRL as u64;
    if let Some((start, stop)) = window {
        session.set_tricore_perf_window(core_index, Some((start, stop)))?;
        println!("window {start:#010x} .. {stop:#010x}: counters cleared");
    }

    let mut core = session.core(core_index)?;
    if run && core.core_halted()? {
        core.run()?;
    }
    let mut previous: Option<[u32; 6]> = None;
    for i in 0..samples {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(interval));
        }
        let mut c = [0u32; 6];
        core.read_32(cctrl, &mut c)?;
        let w = match word {
            Some(address) => format!("  word {:>10}", core.read_word_32(address)?),
            None => String::new(),
        };
        let delta = previous
            .map(|p| {
                format!(
                    "  (+{} clocks, +{} instructions)",
                    c[1].wrapping_sub(p[1]),
                    c[2].wrapping_sub(p[2])
                )
            })
            .unwrap_or_default();
        println!(
            "CCTRL {:#06x}  CCNT {:>10}  ICNT {:>10}  M1 {:>10}  M2 {:>10}  M3 {:>10}{w}{delta}",
            c[0], c[1], c[2], c[3], c[4], c[5]
        );
        previous = Some(c);
    }
    println!(
        "core {}",
        if core.core_halted()? {
            "halted"
        } else {
            "running"
        }
    );
    Ok(())
}
