//! WS63 blinky bring-up diagnostic: reset-halt at the reset vector, single-step
//! through the rt startup, print PC each step, and stop + dump trap CSRs the moment
//! execution leaves flash (>= 0xA00000) or enters the trap-vector region. This pins
//! down exactly which instruction faults and why (mcause/mepc/mtval).

use anyhow::{Result, anyhow};
use probe_rs::config::{Registry, TargetSelector};
use probe_rs::probe::WireProtocol;
use probe_rs::probe::list::Lister;
use probe_rs::{MemoryInterface, Permissions, RegisterId};
use std::time::Duration;

fn rd(core: &mut probe_rs::Core, id: u16) -> u32 {
    core.read_core_reg::<u32>(RegisterId(id)).unwrap_or(0xDEAD_BEEF)
}

fn main() -> Result<()> {
    env_logger::init();
    // Chip description: override with YAML=<path>; defaults to the in-tree WS63 target.
    let yaml_path = std::env::var("YAML")
        .unwrap_or_else(|_| "probe-rs/targets/HiSilicon_WS63.yaml".to_string());
    let yaml = std::fs::read_to_string(&yaml_path)
        .map_err(|e| anyhow!("read chip yaml {yaml_path}: {e} (set YAML=<path> or run from the probe-rs repo root)"))?;
    let mut registry = Registry::from_builtin_families();
    registry.add_target_family_from_yaml(&yaml)?;
    let target = registry.get_target_by_name("WS63")?;

    let lister = Lister::new();
    let probes = lister.list_all();
    let info = probes.first().ok_or_else(|| anyhow!("no probe"))?;
    let mut probe = info.open()?;
    probe.select_protocol(WireProtocol::Swd)?;

    let mut session =
        probe.attach_with_registry(TargetSelector::Specified(target), Permissions::default(), &registry)?;
    let mut core = session.core(0)?;

    // Catch blinky at its true entry: set a HW breakpoint at the flashboot jump
    // target (image_addr + 0x300 = 0x230300), then reset and let the boot chain
    // (mask ROM -> flashboot -> app) run up to it. reset_and_halt alone lands deep
    // in the ROM *after* the app has already crashed, which hides the real fault.
    let entry: u64 = std::env::var("ENTRY")
        .ok()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0023_0300);
    core.reset_and_halt(Duration::from_millis(500))?;
    let rpc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
    println!("after reset_and_halt: pc={rpc:#010x}  (reset vector if halt-on-reset works; ~0x1000b8 if not)");
    dump(&mut core);
    core.set_hw_breakpoint(entry)?;
    println!("set bp @ {entry:#010x}, running...");
    core.run()?;
    match core.wait_for_core_halted(Duration::from_secs(4)) {
        Ok(()) => {
            let pc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
            println!("halted @ pc={pc:#010x} (expected entry {entry:#010x})");
            if pc != entry {
                println!("!! did NOT stop at entry -- boot chain never reached the app entry");
                dump(&mut core);
            }
        }
        Err(e) => {
            println!("bp never hit within timeout: {e}");
            let pc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
            println!("current pc={pc:#010x}");
            dump(&mut core);
            return Ok(());
        }
    }
    core.clear_hw_breakpoint(entry)?;

    // Now run at FULL SPEED to a sequence of milestones in the rt startup path.
    // If a milestone isn't reached in time, the app crashed before it -> dump CSRs
    // (mepc points at the faulting PC, mcause the reason).
    let milestones: &[(&str, u64)] = &[
        ("runtime_init", 0x0023_0d9a),
        ("main", 0x0023_0ca6),
    ];
    for (name, addr) in milestones {
        core.set_hw_breakpoint(*addr)?;
        println!("\n--- run to {name} @ {addr:#010x} ---");
        core.run()?;
        match core.wait_for_core_halted(Duration::from_secs(5)) {
            Ok(()) => {
                let pc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
                println!("  HALTED @ pc={pc:#010x}");
                dump(&mut core);
                if pc != *addr {
                    println!("  !! stopped somewhere other than {name}");
                    core.clear_hw_breakpoint(*addr)?;
                    break;
                }
            }
            Err(e) => {
                println!("  {name} NOT reached: {e}");
                core.halt(Duration::from_millis(500)).ok();
                let pc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
                println!("  crash pc={pc:#010x}");
                dump(&mut core);
                core.clear_hw_breakpoint(*addr)?;
                return Ok(());
            }
        }
        core.clear_hw_breakpoint(*addr)?;
    }

    // main reached: now run FREE and sample PC several times. If blinky is really
    // executing its loop, every sample lands in flash code (0x230xxx); GPIO0 toggles.
    println!("\n=== free-run sampling (blinky loop should keep PC in 0x230xxx) ===");
    for k in 0..16 {
        core.run()?;
        std::thread::sleep(Duration::from_millis(1200));
        core.halt(Duration::from_millis(500))?;
        let pc: u64 = core.read_core_reg(RegisterId(0x7b1))?;
        // GPIO0 output-data reg lives in the GPIO block; just report PC region.
        let region = if (0x23_0000..0x24_0000).contains(&pc) {
            "FLASH-code OK"
        } else if pc >= 0xa0_0000 {
            "SRAM!! (crash)"
        } else {
            "other"
        };
        // GPIO0 block @ 0x4402_8000, DesignWare swporta_dr (data out) at offset 0x00.
        let gpio_dr = core.read_word_32(0x4402_8000).unwrap_or(0xFFFF_FFFF);
        let led = (gpio_dr >> 6) & 1; // GPIO6 = bit 6 of GPIO0-block swporta_dr
        println!("  sample {k}: pc={pc:#010x}  [{region}]  GPIO0.dr={gpio_dr:#010x} led={led}");
    }
    Ok(())
}

fn dump(core: &mut probe_rs::Core) {
    println!("  mstatus={:#010x} mcause={:#010x} mepc={:#010x} mtval={:#010x}",
        rd(core, 0x300), rd(core, 0x342), rd(core, 0x341), rd(core, 0x343));
    println!("  mtvec  ={:#010x} mie   ={:#010x} mip  ={:#010x}",
        rd(core, 0x305), rd(core, 0x304), rd(core, 0x344));
    // GPRs: x1=ra .. x5=t0 .. via RegisterId(0x1000+n)
    println!("  ra={:#010x} sp={:#010x} gp={:#010x} t0={:#010x} a0={:#010x}",
        rd(core, 0x1001), rd(core, 0x1002), rd(core, 0x1003), rd(core, 0x1005), rd(core, 0x100a));
}
