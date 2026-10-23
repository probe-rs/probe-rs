//! Requests a software reset of a TC275 the way an application does (Safety ENDINIT cleared
//! through the Safety watchdog, then `SCU_SWRSTCON.SWRSTREQ`), and prints which debug settings
//! survive it: the trigger units, `EXEVT`, `SWEVT` and `DBGSR` of every core, the trigger
//! switch routing (`CBS_TRCn`) and `SCU_RSTSTAT`.
//!
//! `--arm` first puts debug settings on CPU0 through a session: a breakpoint at
//! `0x8021C008`, a data watchpoint on `0x7000A004`, the halt group routing and `SWEVT`.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example sw_reset -- [--arm]
//! ```

use std::time::Duration;

use probe_rs::{
    Permissions, WatchpointAccess,
    architecture::tricore::bus::{AddressMap, TriCoreBusAccess},
    probe::{DebugProbeSelector, list::Lister},
};

const CSFR: [u32; 3] = [0xF881_0000, 0xF883_0000, 0xF885_0000];
const WDTSCON0: u32 = 0xF003_60F0;
const SWRSTCON: u32 = 0xF003_6060;
const RSTSTAT: u32 = 0xF003_6050;

fn dump(bus: &mut dyn TriCoreBusAccess, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("== {label}");
    println!(
        "RSTSTAT {:#010x}",
        bus.read_u32(AddressMap::Target, RSTSTAT)?
    );
    for (core, &csfr) in CSFR.iter().enumerate() {
        let mut trig = [0u8; 64];
        bus.read_block(AddressMap::Target, csfr + 0xF000, &mut trig)?;
        let units = trig
            .chunks(8)
            .map(|u| {
                let evt = u32::from_le_bytes(u[0..4].try_into().unwrap());
                let adr = u32::from_le_bytes(u[4..8].try_into().unwrap());
                format!("{evt:#x}@{adr:#x}")
            })
            .collect::<Vec<_>>();
        println!(
            "CPU{core}: DBGSR {:#06x} EXEVT {:#04x} SWEVT {:#04x} TRC {:#010x} TR {}",
            bus.read_u32(AddressMap::Target, csfr + 0xFD00)?,
            bus.read_u32(AddressMap::Target, csfr + 0xFD08)?,
            bus.read_u32(AddressMap::Target, csfr + 0xFD10)?,
            bus.read_u32(AddressMap::Target, 0xF000_0420 + 4 * core as u32)?,
            units.join(" ")
        );
    }
    Ok(())
}

/// Clears or sets Safety ENDINIT (password access, then modify access).
fn safety_endinit(
    bus: &mut dyn TriCoreBusAccess,
    endinit: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = bus.read_u32(AddressMap::Target, WDTSCON0)?;
    let password = (value ^ 0xFC) & 0xFFFC;
    let reload = value & 0xFFFF_0000;
    if value & 2 != 0 {
        bus.write_u32(AddressMap::Target, WDTSCON0, reload | password | 1)?;
    }
    bus.write_u32(
        AddressMap::Target,
        WDTSCON0,
        reload | password | 2 | endinit,
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arm = std::env::args().any(|a| a == "--arm");
    probe_rs_infineon::register_plugin();
    let selector = std::env::var("PROBE_RS_PROBE").unwrap_or_else(|_| "058b:0000".to_string());
    let mut session = Lister::new()
        .open(selector.parse::<DebugProbeSelector>()?)?
        .attach("TC275", Permissions::default())?;
    if arm {
        session.set_tricore_halt_group(0, true)?;
        let mut core = session.core(0)?;
        core.set_hw_breakpoint(0x8021_C008)?;
        core.set_watchpoint(0x7000_A004, 4, WatchpointAccess::Write)?;
    }
    let bus = session.get_tricore_bus()?;
    if arm {
        bus.write_u32(AddressMap::Target, CSFR[0] + 0xFD10, 0x2A)?;
    }
    dump(bus, "before the software reset")?;

    safety_endinit(bus, 0)?;
    // SWRSTREQ: the transfer may fail as the device resets.
    let _ = bus.write_u32(AddressMap::Target, SWRSTCON, 1 << 1);
    // The connection may refuse accesses for a while after the reset.
    let start = std::time::Instant::now();
    let mut failures = 0;
    while bus.read_u32(AddressMap::Target, RSTSTAT).is_err() {
        failures += 1;
        if start.elapsed() > Duration::from_secs(20) {
            return Err("no access 20 s after the reset".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "accessible again after {} ms ({failures} failed reads)",
        start.elapsed().as_millis()
    );
    dump(bus, "after the software reset")?;
    // Leave the registers as the reset left them, to read them again from another session.
    std::mem::forget(session);
    Ok(())
}
