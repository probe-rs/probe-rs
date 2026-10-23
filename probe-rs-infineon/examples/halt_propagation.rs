//! Shows how a halt of CPU0 affects CPU1 and CPU2 (DBGSR sampled every 25 ms for 1 s).
//!
//! It needs CPU0 halted at `main` of `demo_flash.elf` (run `start_in_flash` first). Nothing is
//! written to flash.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example halt_propagation -- path/to/demo_flash.elf
//! ```

use std::time::{Duration, Instant};

use probe_rs::{
    Permissions,
    architecture::tricore::bus::AddressMap,
    probe::{DebugProbeSelector, list::Lister},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn dbgsr_address(core: u32) -> u32 {
    0xF881_FD00 + core * 0x2_0000
}

fn sample(session: &mut probe_rs::Session, what: &str) -> Result<()> {
    println!("-- {what}");
    let start = Instant::now();
    let mut last = [u32::MAX; 3];
    while start.elapsed() < Duration::from_secs(1) {
        let bus = session.get_tricore_bus()?;
        let mut now = [0; 3];
        for (core, value) in now.iter_mut().enumerate() {
            *value = bus.read_u32(AddressMap::Target, dbgsr_address(core as u32))?;
        }
        if now != last {
            let state = |v: u32| {
                if (v >> 1) & 3 == 1 {
                    "halted "
                } else {
                    "running"
                }
            };
            println!(
                "{:>5} ms  CPU0 {} {:#06x}  CPU1 {} {:#06x}  CPU2 {} {:#06x}",
                start.elapsed().as_millis(),
                state(now[0]),
                now[0],
                state(now[1]),
                now[1],
                state(now[2]),
                now[2]
            );
            last = now;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

fn main() -> Result<()> {
    let elf_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\work\aurix_tcf_demo\build\demo_flash.elf".to_string());
    let elf = std::fs::read(&elf_path)?;
    let square = probe_rs_debug::DebugInfo::from_file(&elf_path)?
        .get_breakpoint_location(
            typed_path::TypedPath::derive(r"C:\work\aurix_tcf_demo\src\demo.c"),
            17,
            None,
        )?
        .address;
    let _ = elf;

    probe_rs_infineon::register_plugin();
    let selector: DebugProbeSelector = std::env::var("PROBE_RS_PROBE")
        .unwrap_or_else(|_| "058b:0000".to_string())
        .parse()?;
    let probe = Lister::new().open(selector)?;
    let mut session = probe.attach("TC275", Permissions::default())?;

    sample(
        &mut session,
        "start (CPU0 halted at main by start_in_flash)",
    )?;

    // A halt request (DBGSR) on CPU1.
    session.core(1)?.halt(Duration::from_millis(200))?;
    sample(&mut session, "after a halt request on CPU1")?;
    session.core(1)?.run()?;
    sample(&mut session, "after running CPU1 again")?;

    // A hardware breakpoint hit on CPU0.
    {
        let mut core = session.core(0)?;
        core.set_hw_breakpoint(square)?;
        core.run()?;
    }
    sample(&mut session, "CPU0 runs into the breakpoint in square")?;
    {
        let mut core = session.core(0)?;
        core.clear_all_hw_breakpoints()?;
        core.step()?;
    }
    sample(&mut session, "after one step on CPU0")?;
    Ok(())
}
