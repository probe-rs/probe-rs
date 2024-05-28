use bytesize::ByteSize;
use probe_rs::config::MemoryRegion;

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
        let output = std::io::stdout().lock();

        match self.subcommand {
            Subcommand::List => print_families(output),
            Subcommand::Info { name } => print_chip_info(output, &name),
        }
    }
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_families(mut output: impl std::io::Write) -> anyhow::Result<()> {
    writeln!(output, "Available chips:")?;
    for family in probe_rs::config::families() {
        writeln!(output, "{}", &family.name)?;
        writeln!(output, "    Variants:")?;
        for variant in family.variants() {
            writeln!(output, "        {}", variant.name)?;
        }
    }
    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_chip_info(mut output: impl std::io::Write, name: &str) -> anyhow::Result<()> {
    writeln!(output, "{}", name)?;
    let target = probe_rs::config::get_target_by_name(name)?;
    writeln!(output, "Cores ({}):", target.cores.len())?;
    for core in target.cores {
        writeln!(
            output,
            "    - {} ({:?})",
            core.name.to_ascii_lowercase(),
            core.core_type
        )?;
    }

    fn get_range_len(range: &std::ops::Range<u64>) -> u64 {
        range.end - range.start
    }

    for memory in target.memory_map {
        let range = memory.address_range();
        let size = ByteSize(get_range_len(&range)).to_string_as(true);
        let kind = match memory {
            MemoryRegion::Ram(_) => "RAM",
            MemoryRegion::Generic(_) => "Generic",
            MemoryRegion::Nvm(_) => "NVM",
        };
        writeln!(output, "{kind}: {range:#010x?} ({size})")?
    }
    Ok(())
}

#[test]
fn single_chip_output() {
    let mut buff = Vec::new();
    print_chip_info(&mut buff, "nrf52840_xxaa").unwrap();

    // output should be valid utf8
    let output = String::from_utf8(buff).unwrap();

    insta::assert_snapshot!(output);
}

#[test]
fn multiple_chip_output() {
    let mut buff = Vec::new();
    let error = print_chip_info(&mut buff, "nrf52").unwrap_err();

    insta::assert_snapshot!(error.to_string());
}
