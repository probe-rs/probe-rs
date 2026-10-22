//! Connects through a TAS server and prints the device identification and the core states,
//! without halting or writing anything.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example tas_info -- ["DAS JTAG AURDUINO 46YPEADW"]
//! ```

use probe_rs::{
    architecture::tricore::{
        bus::{AddressMap, TriCoreBusAccess},
        ocds,
    },
    probe::DebugProbe,
};
use probe_rs_infineon::tas::{Endpoint, TasProbe};

const CBS_JTAGID: u32 = 0xF000_0464;
const SCU_CHIPID: u32 = 0xF003_6140;
const CSFR_BASE: u32 = 0xF881_0000;
const CSFR_STRIDE: u32 = 0x2_0000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::env::args().nth(1);
    let mut probe = TasProbe::new(Endpoint {
        target,
        ..Endpoint::default()
    });
    probe.attach()?;
    println!("TAS target : {}", probe.target_name());
    println!("JTAG ID    : {:#010x}", probe.jtag_id()?);
    println!(
        "CBS_JTAGID : {:#010x}",
        probe.read_u32(AddressMap::Target, CBS_JTAGID)?
    );
    println!(
        "SCU_CHIPID : {:#010x}",
        probe.read_u32(AddressMap::Target, SCU_CHIPID)?
    );
    for core in 0..3 {
        let base = CSFR_BASE + core * CSFR_STRIDE;
        let dbgsr = probe.read_u32(AddressMap::Target, base + ocds::DBGSR as u32)?;
        let state = if ocds::is_halted(dbgsr) {
            "halted"
        } else if ocds::is_running(dbgsr) {
            "running"
        } else {
            "?"
        };
        let pc = probe.read_u32(AddressMap::Target, base + 0xFE08)?;
        println!("CPU{core} DBGSR : {dbgsr:#010x} ({state}), PC {pc:#010x}");
    }
    probe.detach()?;
    Ok(())
}
