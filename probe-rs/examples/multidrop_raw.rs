//! This example demonstrates how to do raw DP register access on multidrop systems.

use anyhow::Result;
use probe_rs::{
    architecture::arm::{
        dp::{DpAddress, DpRegisterAddress},
        sequences::DefaultArmSequence,
    },
    probe::{Probe, list::Lister},
};

fn main() -> Result<()> {
    async_io::block_on(async move {
        env_logger::init();

        // Get a list of all available debug probes.

        let probe_lister = Lister::new();

        let probes = probe_lister.list_all().await;

        // Use the first probe found.
        let mut probe: Probe = probes[0].open()?;

        // Specify the multidrop DP address of the first core,
        // this is used for the initial connection.
        let core0 = DpAddress::Multidrop(0x01002927);

        probe.set_speed(100)?;
        probe.attach_to_unspecified()?;
        let mut iface = probe
            .try_into_arm_interface(DefaultArmSequence::create())
            .map_err(|(_probe, err)| err)?;

        iface.select_debug_port(core0)?;

        // This is an example on how to do raw DP register access with multidrop.
        // This reads DPIDR and TARGETID of both cores in a RP2040. This chip is
        // unconventional because each core has its own DP.

        let core1 = DpAddress::Multidrop(0x11002927);
        const DPIDR: DpRegisterAddress = DpRegisterAddress {
            address: 0x0,
            bank: Some(0x0),
        };
        const TARGETID: DpRegisterAddress = DpRegisterAddress {
            address: 0x4,
            bank: Some(0x2),
        };

        println!(
            "core0 DPIDR:    {:08x}",
            iface.read_raw_dp_register(core0, DPIDR)?
        );
        println!(
            "core0 TARGETID: {:08x}",
            iface.read_raw_dp_register(core0, TARGETID)?
        );
        println!(
            "core1 DPIDR:    {:08x}",
            iface.read_raw_dp_register(core1, DPIDR)?
        );
        println!(
            "core1 TARGETID: {:08x}",
            iface.read_raw_dp_register(core1, TARGETID)?
        );

        Ok(())
    })
}
