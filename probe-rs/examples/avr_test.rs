use probe_rs::Probe;
use probe_rs::CoreRegisterAddress;

fn main() -> Result<(), probe_rs::Error> {
    pretty_env_logger::init();
    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let mut probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("avr128da48")?;

    // Select a core.
    let mut core = session.core(0)?;

    // Halt the attached core.
    let pc = core.read_core_reg(CoreRegisterAddress(34));
    println!("PC : {:?}", pc);

    let core_info = core.halt(std::time::Duration::from_millis(300))?;
    println!("CoreInfo : {:?}", core_info);
    let core_info = core.step()?;
    println!("CoreInfo : {:?}", core_info);
    let core_info = core.step()?;
    println!("CoreInfo : {:?}", core_info);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let core_info = core.step()?;
    println!("CoreInfo : {:?}", core_info);
    let core_info = core.step()?;
    println!("CoreInfo : {:?}", core_info);
    let core_info = core.step()?;
    println!("CoreInfo : {:?}", core_info);


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

