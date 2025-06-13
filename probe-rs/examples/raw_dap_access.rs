//! This example demonstrates how to use the raw DAP access API to perform a chip recovery operation on a nRF52840 target.

use anyhow::Result;
use probe_rs::{
    architecture::arm::{FullyQualifiedApAddress, dp::DpAddress, sequences::DefaultArmSequence},
    probe::list::Lister,
};

fn main() -> Result<()> {
    async_io::block_on(async move {
        env_logger::init();

        let lister = Lister::new();

        // Get a list of all available debug probes.
        let probes = lister.list_all().await;

        // Use the first probe found.
        let mut probe = probes[0].open()?;

        probe.attach_to_unspecified()?;
        let mut iface = probe
            .try_into_arm_debug_interface(DefaultArmSequence::create())
            .unwrap();

        iface.select_debug_port(DpAddress::Default)?;

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
    })
}
