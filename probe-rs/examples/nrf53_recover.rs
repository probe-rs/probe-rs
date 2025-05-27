//! This example demonstrates how to do a "recover" operation (erase+unlock a locked chip) on an nRF5340 target.

use anyhow::Result;
use probe_rs::{
    architecture::arm::{
        FullyQualifiedApAddress,
        ap::{ApRegister, IDR},
        dp::DpAddress,
        sequences::DefaultArmSequence,
    },
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
            .try_into_arm_interface(DefaultArmSequence::create())
            .unwrap();

        iface.select_debug_port(DpAddress::Default).unwrap();

        const APP_MEM: FullyQualifiedApAddress = FullyQualifiedApAddress::v1_with_default_dp(0);
        const NET_MEM: FullyQualifiedApAddress = FullyQualifiedApAddress::v1_with_default_dp(1);
        const APP_CTRL: FullyQualifiedApAddress = FullyQualifiedApAddress::v1_with_default_dp(2);
        const NET_CTRL: FullyQualifiedApAddress = FullyQualifiedApAddress::v1_with_default_dp(3);

        const ERASEALL: u64 = 0x04;
        const ERASEALLSTATUS: u64 = 0x08;

        for ap in &[APP_MEM, NET_MEM, APP_CTRL, NET_CTRL] {
            println!(
                "IDR {:?} {:x}",
                ap,
                iface.read_raw_ap_register(ap, IDR::ADDRESS)?
            );
        }

        for ap in &[APP_CTRL, NET_CTRL] {
            // Start erase
            iface.write_raw_ap_register(ap, ERASEALL, 1)?;
            // Wait for erase done
            while iface.read_raw_ap_register(ap, ERASEALLSTATUS)? != 0 {}
        }

        Ok(())
    })
}
