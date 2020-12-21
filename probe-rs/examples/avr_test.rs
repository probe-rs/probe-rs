use anyhow::{anyhow, Context, Result};
use probe_rs::config::{Chip, MemoryRegion, RamRegion};
use probe_rs::Target;
use probe_rs::CoreType;
use probe_rs::{config::TargetSelector, MemoryInterface, Probe, WireProtocol};
use std::borrow::Cow;


fn main() -> Result<()> {
    pretty_env_logger::init();
    log::debug!("Test");
    let list = Probe::list_all();
    println!("Probe list: {:?}", list);
    let probe = list[0].open()?;
    println!("Probe: {:?}", probe);

    let com = probe.attach(TargetSelector::Specified(Target::new(
        &Chip {
            name: Cow::Borrowed("avr128da48"),
            part: None,
            memory_map: Cow::Borrowed(&[MemoryRegion::Ram(RamRegion {
                range: 0..0x1000,
                is_boot_memory: false,
            })]),
            flash_algorithms: Cow::Owned(vec![Cow::Borrowed("AVR")]),
        },
        vec![],
        CoreType::Avr,
    )));
    println!("com: {:?}", com);

    Ok(())
}
