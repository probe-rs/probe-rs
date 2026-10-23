//! Reads target memory straight from the bus and prints it as hex bytes. Read only: unlike
//! `probe-rs read`, no core is halted or resumed, so a debugger session keeps its cores as they
//! are. Software breakpoints show as `DEBUG` instructions (`00 a0`, `0d 00 00 01`).
//!
//! ```text
//! cargo run -p probe-rs-infineon --example bus_read -- <address> <length>
//! ```

use probe_rs::{
    Permissions,
    architecture::tricore::bus::AddressMap,
    probe::{DebugProbeSelector, list::Lister},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: bus_read <address> <length>";
    let address = args.next().ok_or(usage)?;
    let address = u32::from_str_radix(address.trim_start_matches("0x"), 16)?;
    let length: usize = args.next().ok_or(usage)?.parse()?;

    probe_rs_infineon::register_plugin();
    // `PROBE_RS_PROBE` selects another probe, for example the MCD probe.
    let selector = std::env::var("PROBE_RS_PROBE").unwrap_or_else(|_| "058b:0000".to_string());
    let mut session = Lister::new()
        .open(selector.parse::<DebugProbeSelector>()?)?
        .attach("TC275", Permissions::default())?;
    let mut data = vec![0; length];
    session
        .get_tricore_bus()?
        .read_block(AddressMap::Target, address, &mut data)?;
    let text = data
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{text}");
    Ok(())
}
