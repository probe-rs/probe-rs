use byte_unit::Byte;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
/// Inspect internal registry of supported chips
enum Subcommand {
    /// Lists all the available families and their chips with their full.
    #[clap(name = "list")]
    List,
    /// Shows chip properties of a specific chip
    #[clap(name = "info")]
    Info {
        /// The name of the chip to display.
        name: String,
    },
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        match self.subcommand {
            Subcommand::List => print_families().map_err(Into::into),
            Subcommand::Info { name } => print_chip_info(name),
        }
    }
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_families() -> anyhow::Result<()> {
    println!("Available chips:");
    for family in probe_rs::config::families()? {
        println!("{}", &family.name);
        println!("    Variants:");
        for variant in family.variants() {
            println!("        {}", variant.name);
        }
    }
    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_chip_info(name: impl AsRef<str>) -> anyhow::Result<()> {
    println!("{}", name.as_ref());
    let target = probe_rs::config::get_target_by_name(name)?;
    println!("Cores ({}):", target.cores.len());
    for core in target.cores {
        println!(
            "    - {} ({:?})",
            core.name.to_ascii_lowercase(),
            core.core_type
        );
    }

    fn get_range_len(range: &std::ops::Range<u64>) -> u64 {
        range.end - range.start
    }

    for memory in target.memory_map {
        match memory {
            probe_rs::config::MemoryRegion::Ram(region) => println!(
                "RAM: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
            probe_rs::config::MemoryRegion::Generic(region) => println!(
                "Generic: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
            probe_rs::config::MemoryRegion::Nvm(region) => println!(
                "NVM: {:#010x?} ({})",
                &region.range,
                Byte::from_bytes(get_range_len(&region.range) as u128).get_appropriate_unit(true)
            ),
        };
    }
    Ok(())
}
