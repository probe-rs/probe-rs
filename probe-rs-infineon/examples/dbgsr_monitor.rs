//! Prints every change of the run state (DBGSR) of CPU0-2 for a while, with the wall clock.
//! Read only.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example dbgsr_monitor -- <seconds>
//! ```

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use probe_rs::{
    Permissions,
    architecture::tricore::bus::AddressMap,
    probe::{DebugProbeSelector, list::Lister},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args().nth(1).map_or(Ok(20), |s| s.parse())?;
    probe_rs_infineon::register_plugin();
    let selector: DebugProbeSelector = "058b:0000".parse()?;
    let mut session = Lister::new()
        .open(selector)?
        .attach("TC275", Permissions::default())?;
    let start = Instant::now();
    let mut last = [u32::MAX; 3];
    while start.elapsed() < Duration::from_secs(seconds) {
        let bus = session.get_tricore_bus()?;
        let mut now = [0; 3];
        for (core, value) in now.iter_mut().enumerate() {
            *value = bus.read_u32(AddressMap::Target, 0xF881_FD00 + core as u32 * 0x2_0000)?;
        }
        if now != last {
            let ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() % 100_000;
            let state = |v: u32| if (v >> 1) & 3 == 1 { "H" } else { "r" };
            println!(
                "{:>6}.{:03} CPU0 {} {:#06x}  CPU1 {} {:#06x}  CPU2 {} {:#06x}",
                ms / 1000,
                ms % 1000,
                state(now[0]),
                now[0],
                state(now[1]),
                now[1],
                state(now[2]),
                now[2]
            );
            last = now;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
