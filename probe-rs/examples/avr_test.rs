use probe_rs::Probe;

fn main() -> Result<(), probe_rs::Error> {
    pretty_env_logger::init();
    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("avr128da48")?;

    // Select a core.
    //let mut core = session.core(0)?;

    // Halt the attached core.
    //core.halt(std::time::Duration::from_millis(300))?;

    Ok(())
}

    //let com = probe.attach(TargetSelector::Specified(Target::new(
    //    &Chip {
    //        name: Cow::Borrowed("avr128da48"),
    //        part: None,
    //        memory_map: Cow::Borrowed(&[MemoryRegion::Ram(RamRegion {
    //            range: 0..0x1000,
    //            is_boot_memory: false,
    //        })]),
    //        flash_algorithms: Cow::Owned(vec![Cow::Borrowed("AVR")]),
    //    },
    //    vec![],
    //    CoreType::Avr,
    //)));
    //println!("com: {:?}", com);

