use anyhow::Result;
use probe_rs::{
    architecture::arm::{ApAddress, DpAddress},
    Probe,
};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let mut probe = probes[0].open()?;

    probe.attach_to_unspecified()?;
    let mut iface = probe.try_into_arm_interface().unwrap();

    // This is an example on how to do a "recover" operation (erase+unlock a locked chip)
    // on an nRF52840 target.

    const APP_MEM: ApAddress = ApAddress {
        ap: 0,
        dp: DpAddress::Default,
    };
    const NET_MEM: ApAddress = ApAddress {
        ap: 1,
        dp: DpAddress::Default,
    };
    const APP_CTRL: ApAddress = ApAddress {
        ap: 2,
        dp: DpAddress::Default,
    };
    const NET_CTRL: ApAddress = ApAddress {
        ap: 3,
        dp: DpAddress::Default,
    };

    const ERASEALL: u8 = 0x04;
    const ERASEALLSTATUS: u8 = 0x08;
    const IDR: u8 = 0xFC;

    for &ap in &[APP_MEM, NET_MEM, APP_CTRL, NET_CTRL] {
        println!("IDR {:?} {:x}", ap, iface.read_raw_ap_register(ap, IDR)?);
    }

    for &ap in &[APP_CTRL, NET_CTRL] {
        // Start erase
        iface.write_raw_ap_register(ap, ERASEALL, 1)?;
        // Wait for erase done
        while iface.read_raw_ap_register(ap, ERASEALLSTATUS)? != 0 {}
    }

    Ok(())
}
