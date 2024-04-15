use anyhow::Result;
use probe_rs::{
    architecture::arm::DpAddress,
    probe::{list::Lister, Probe},
};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.

    let probe_lister = Lister::new();

    let probes = probe_lister.list_all();

    // Use the first probe found.
    let mut probe: Probe = probes[0].open()?;

    // Specify the multidrop DP address of the first core,
    // this is used for the initial connection.
    let core0 = DpAddress::Multidrop(0x01002927);

    probe.set_speed(100)?;
    probe.attach_to_unspecified()?;
    let mut iface = probe
        .try_into_arm_interface()
        .map_err(|(_probe, err)| err)?
        .initialize_unspecified(core0)
        .map_err(|(_interface, err)| err)?;

    // This is an example on how to do raw DP register access with multidrop.
    // This reads DPIDR and TARGETID of both cores in a RP2040. This chip is
    // unconventional because each core has its own DP.

    let core1 = DpAddress::Multidrop(0x11002927);

    println!(
        "core0 DPIDR:    {:08x}",
        iface.read_raw_dp_register(core0, 0x00)?
    );
    println!(
        "core0 TARGETID: {:08x}",
        iface.read_raw_dp_register(core0, 0x24)?
    );
    println!(
        "core1 DPIDR:    {:08x}",
        iface.read_raw_dp_register(core1, 0x00)?
    );
    println!(
        "core1 TARGETID: {:08x}",
        iface.read_raw_dp_register(core1, 0x24)?
    );

    Ok(())
}
