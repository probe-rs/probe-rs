//! This example demonstrates how to use the raw DAP access API to perform a chip recovery operation on a nRF52840 target.

use anyhow::Result;
use probe_rs::{
    architecture::arm::{dp::DpAddress, sequences::DefaultArmSequence, FullyQualifiedApAddress},
    probe::list::Lister,
};

fn main() -> Result<()> {
    pretty_env_logger::init();

    let lister = Lister::new();

    // Get a list of all available debug probes.
    let probes = lister.list_all();

    // Use the first probe found.
    let mut probe = probes[0].open()?;

    probe.attach_to_unspecified()?;
    let iface = probe.try_into_arm_interface().unwrap();

    let mut iface = iface
        .initialize(DefaultArmSequence::create(), DpAddress::Default)
        .map_err(|(_interface, e)| e)?;

    let port = &FullyQualifiedApAddress::v1_with_default_dp(1);

    const RESET: u64 = 0;
    const ERASEALL: u64 = 4;
    const ERASEALLSTATUS: u64 = 8;

    // Reset
    iface.write_raw_ap_register(port, RESET, 1)?;
    iface.write_raw_ap_register(port, RESET, 0)?;

    // Start erase
    iface.write_raw_ap_register(port, ERASEALL, 1)?;

    // Wait for erase done
    while iface.read_raw_ap_register(port, ERASEALLSTATUS)? != 0 {}

    // Reset again
    iface.write_raw_ap_register(port, RESET, 1)?;
    iface.write_raw_ap_register(port, RESET, 0)?;

    Ok(())
}
