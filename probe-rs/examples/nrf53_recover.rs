use anyhow::Result;
use probe_rs::Probe;

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

    const APP_MEM: u8 = 0;
    const NET_MEM: u8 = 1;
    const APP_CTRL: u8 = 2;
    const NET_CTRL: u8 = 3;

    const RESET: u8 = 0x00;
    const ERASEALL: u8 = 0x04;
    const ERASEALLSTATUS: u8 = 0x08;
    const APPROTECT_DISABLE: u8 = 0x10;
    const SECUREAPPROTECT_DISABLE: u8 = 0x14;
    const ERASEPROTECT_STATUS: u8 = 0x18;
    const ERASEPROTECT_DISABLE: u8 = 0x1C;
    const IDR: u8 = 0xFC;

    for &ap in &[APP_MEM, NET_MEM, APP_CTRL, NET_CTRL] {
        println!("IDR {} {:x}", ap, iface.read_raw_ap_register(ap, IDR)?);
    }

    for &ap in &[APP_CTRL, NET_CTRL] {
        // Start erase
        iface.write_raw_ap_register(ap, ERASEALL, 1)?;
        // Wait for erase done
        while iface.read_raw_ap_register(ap, ERASEALLSTATUS)? != 0 {}
    }

    Ok(())
}
