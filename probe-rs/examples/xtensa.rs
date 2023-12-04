use anyhow::Result;
use probe_rs::config::ScanChainElement;
use probe_rs::{Lister, MemoryInterface, Probe};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probe_lister = Lister::new();

    let probes = probe_lister.list_all();

    // Use the first probe found.
    let mut probe: Probe = probes[0].open(&probe_lister)?;

    probe.set_speed(100)?;
    probe.select_protocol(probe_rs::WireProtocol::Jtag)?;
    // scan chain for an esp32s3
    probe.set_scan_chain(vec![
        ScanChainElement {
            ir_len: Some(5),
            name: Some("main".to_owned()),
        },
        ScanChainElement {
            ir_len: Some(5),
            name: Some("second".to_owned()),
        },
    ])?;
    probe.attach_to_unspecified()?;
    let mut iface = probe.try_into_xtensa_interface().unwrap();

    iface.enter_ocd_mode()?;

    assert!(iface.is_in_ocd_mode()?);

    iface.halt()?;

    const SYSTEM_BASE_REGISTER: u32 = 0x600C_0000;
    const SYSTEM_DATE_REGISTER: u32 = SYSTEM_BASE_REGISTER | 0x0FFC;
    let date = iface.read_word_32(SYSTEM_DATE_REGISTER as u64)?;

    iface.leave_ocd_mode()?;

    println!("SYSTEM peripheral date: {:08x}", date);

    Ok(())
}
