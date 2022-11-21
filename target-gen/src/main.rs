pub mod algorithm_binary;
pub mod fetch;
pub mod flash_device;
pub mod generate;
pub mod parser;

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use parser::extract_flash_algo;
use probe_rs::{
    config::{
        Chip, ChipFamily, Core, MemoryRegion, NvmRegion, RamRegion,
        TargetDescriptionSource::BuiltIn,
    },
    CoreType,
};
use probe_rs_target::{ArmCoreAccessOptions, CoreAccessOptions};
use simplelog::*;
use std::{
    env::current_dir,
    fs::{create_dir, File, OpenOptions},
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

#[derive(clap::Parser)]
enum TargetGen {
    /// Generate target description from ARM CMSIS-Packs
    Pack {
        #[clap(
            name = "INPUT",
            value_parser,
            help = "A Pack file or the unziped Pack directory."
        )]
        input: PathBuf,
        #[clap(
            name = "OUTPUT",
            value_parser,
            help = "An output directory where all the generated .yaml files are put in."
        )]
        output_dir: PathBuf,
    },
    /// Generates from the entries listed in the ARM root VIDX/PIDX at <https://www.keil.com/pack/Keil.pidx>.
    /// This will only download and generate target descriptions for chip families that are already supported by probe-rs, to avoid generating a lot of unsupportable chip families.
    /// Please use the `pack` subcommand to generate target descriptions for other chip families.
    Arm {
        /// Only download and generate target descriptions for the chip families that start with the specified name, e.g. `STM32H7` or `LPC55S69`.
        #[clap(
            long = "list",
            short = 'l',
            help = "Optionally, list the names of all pack files available in <https://www.keil.com/pack/Keil.pidx>"
        )]
        list: bool,
        /// Only download and generate target descriptions for the pack files that start with the specified name`.
        #[clap(
            long = "filter",
            short = 'f',
            help = "Optionally, filter the pack files that start with the specified name,\ne.g. `STM32H7xx` or `LPC55S69_DFP`.\nSee `target-gen arm --list` for a list of available Pack files"
        )]
        pack_filter: Option<String>,
        #[clap(
            name = "OUTPUT",
            value_parser,
            help = "An output directory where all the generated .yaml files are put in."
        )]
        output_dir: Option<PathBuf>,
    },
    /// Extract a flash algorithm from an ELF file
    Elf {
        /// ELF file containing a flash algorithm
        #[clap(value_parser)]
        elf: PathBuf,
        /// Name of the extracted flash algorithm
        #[clap(long = "name", short = 'n')]
        name: Option<String>,
        /// Update an existing flash algorithm
        #[clap(long = "update", short = 'u', requires = "output")]
        update: bool,
        /// Output file, if provided, the generated target description will be written to this file.
        #[clap(value_parser)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let logger = TermLogger::init(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    );
    if logger.is_err() {
        eprintln!("Logging backend could not be initialized.");
    }

    let options = TargetGen::parse();

    let t = std::time::Instant::now();

    match options {
        TargetGen::Pack { input, output_dir } => cmd_pack(&input, &output_dir)?,
        TargetGen::Elf {
            elf,
            output,
            update,
            name,
        } => cmd_elf(elf, output, update, name)?,
        TargetGen::Arm {
            output_dir,
            pack_filter: chip_family,
            list,
        } => cmd_arm(output_dir, chip_family, list)?,
    }

    println!("Finished in {:?}", t.elapsed());

    Ok(())
}

/// Prepare a target config based on an ELF file containing a flash algorithm.
fn cmd_elf(
    file: PathBuf,
    output: Option<PathBuf>,
    update: bool,
    name: Option<String>,
) -> Result<()> {
    let elf_file = File::open(&file)?;

    let mut algorithm = extract_flash_algo(elf_file, &file, true)?;

    if let Some(name) = name {
        algorithm.name = name;
    }

    if update {
        // Update an existing target file

        let target_description_file = output.unwrap(); // Argument is checked by structopt, so we now its present.

        let target_description = File::open(&target_description_file).context(format!(
            "Unable to open target specification '{}'",
            target_description_file.display()
        ))?;

        let mut family: ChipFamily = serde_yaml::from_reader(target_description)?;

        let algorithm_to_update = family
            .flash_algorithms
            .iter()
            .position(|old_algorithm| old_algorithm.name == algorithm.name);

        match algorithm_to_update {
            None => bail!("Unable to update flash algorithm in target description file '{}'. Did not find an existing algorithm with name '{}'", target_description_file.display(), &algorithm.name),
            Some(index) => {
                let current = &family.flash_algorithms[index];

                // if a load address was specified, use it in the replacement
                if let Some(load_addr)  = current.load_address {
                    algorithm.load_address = Some(load_addr);
                    algorithm.data_section_offset = algorithm.data_section_offset.saturating_sub(load_addr);
                }
                // core access cannot be determined, use the current value
                algorithm.cores = current.cores.clone();
                algorithm.description = current.description.clone();

                family.flash_algorithms[index] = algorithm
            },
        }

        let target_description = File::create(&target_description_file)?;
        serialize_to_yaml_file(&family, &target_description)?;
    } else {
        // Create a complete target specification, with place holder values
        let algorithm_name = algorithm.name.clone();
        algorithm.cores = vec!["main".to_owned()];

        let chip_family = ChipFamily {
            name: "<family name>".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![Chip {
                cores: vec![Core {
                    name: "main".to_owned(),
                    core_type: CoreType::Armv6m,
                    core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                        ap: 0,
                        psel: 0,
                        debug_base: None,
                        cti_base: None,
                    }),
                }],
                part: None,
                name: "<chip name>".to_owned(),
                memory_map: vec![
                    MemoryRegion::Nvm(NvmRegion {
                        is_boot_memory: false,
                        range: 0..0x2000,
                        cores: vec!["main".to_owned()],
                        name: None,
                    }),
                    MemoryRegion::Ram(RamRegion {
                        is_boot_memory: true,
                        range: 0x1_0000..0x2_0000,
                        cores: vec!["main".to_owned()],
                        name: None,
                    }),
                ],
                flash_algorithms: vec![algorithm_name],
            }],
            flash_algorithms: vec![algorithm],
            source: BuiltIn,
        };

        match output {
            Some(output) => {
                // Ensure we don't overwrite an existing file
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)
                    .context(format!(
                        "Failed to create target file '{}'.",
                        output.display()
                    ))?;
                serialize_to_yaml_file(&chip_family, &file)?;
            }
            None => println!("{}", serde_yaml::to_string(&chip_family)?),
        }
    }

    Ok(())
}

/// Handle the pack subcommand. `input` is either the path
/// to a CMSIS-Pack file, or a directory containing at least one .pdsc file.
///
/// The generated target description will be placed in `out_dir`.
fn cmd_pack(input: &Path, out_dir: &Path) -> Result<()> {
    ensure!(
        input.exists(),
        "No such file or directory: {}",
        input.display()
    );

    if !out_dir.exists() {
        create_dir(out_dir).context(format!(
            "Failed to create output directory '{}'.",
            out_dir.display()
        ))?;
    }

    let mut families = Vec::<ChipFamily>::new();

    if input.is_file() {
        generate::visit_file(input, &mut families)
            .context(format!("Failed to process file {}.", input.display()))?;
    } else {
        // Look for the .pdsc file in the given dir and it's child directories.
        generate::visit_dirs(input, &mut families)
            .context("Failed to generate target configuration.")?;

        // Check that we found at least a single .pdsc file
        ensure!(
            !families.is_empty(),
            "Unable to find any .pdsc files in the provided input directory."
        );
    }

    let mut generated_files = Vec::with_capacity(families.len());

    for family in &families {
        let path = out_dir.join(family.name.clone().replace(' ', "_") + ".yaml");
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create file '{}'.", path.display()))?;

        serialize_to_yaml_file(family, &file)?;

        generated_files.push(path);
    }

    println!("Generated {} target definition(s):", generated_files.len());

    for file in generated_files {
        println!("\t{}", file.display());
    }

    Ok(())
}

/// Handle the arm subcommand.
/// Generated target descriptions will be placed in `out_dir`.
fn cmd_arm(out_dir: Option<PathBuf>, chip_family: Option<String>, list: bool) -> Result<()> {
    if list {
        let mut packs = crate::fetch::get_vidx()?;
        println!("Available ARM CMSIS Pack files:");
        packs.pdsc_index.sort_by(|a, b| a.name.cmp(&b.name));
        for pack in packs.pdsc_index.iter() {
            println!("\t{}", pack.name);
        }
        return Ok(());
    }

    let out_dir = if let Some(target_dir) = out_dir {
        target_dir.as_path().to_owned()
    } else {
        log::info!("No output directory specified. Using current directory.");
        current_dir()?
    };

    if !out_dir.exists() {
        create_dir(&out_dir).context(format!(
            "Failed to create output directory '{}'.",
            out_dir.display()
        ))?;
    }

    let mut families = Vec::<ChipFamily>::new();

    generate::visit_arm_files(&mut families, chip_family)?;

    let mut generated_files = Vec::with_capacity(families.len());

    for family in &families {
        let path = out_dir.join(family.name.clone().replace(' ', "_") + ".yaml");
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create file '{}'.", path.display()))?;
        serialize_to_yaml_file(family, &file)?;

        generated_files.push(path);
    }

    println!("Generated {} target definition(s):", generated_files.len());

    for file in generated_files {
        println!("\t{}", file.display());
    }

    Ok(())
}

/// Some optimizations to improve the readability of the `serde_yaml` output:
/// - If `Option<T>` is `None`, it is serialized as `null` ... we want to omit it.
/// - If `Vec<T>` is empty, it is serialized as `[]` ... we want to omit it.
/// - `serde_yaml` serializes hex formatted integers as single quoted strings, e.g. '0x1234' ... we need to remove the single quotes so that it round-trips properly.
fn serialize_to_yaml_file(family: &ChipFamily, file: &File) -> Result<(), anyhow::Error> {
    let yaml_string = serde_yaml::to_string(&family)?;
    let mut reader = std::io::BufReader::new(yaml_string.as_bytes());
    let mut reader_line = String::new();
    let mut writer = std::io::BufWriter::new(file);
    while reader.read_line(&mut reader_line)? > 0 {
        if reader_line.ends_with(": null\n")
            || reader_line.ends_with(": []\n")
            || reader_line.ends_with(": false\n")
        {
            // Skip the line
        } else if (reader_line.contains("'0x") || reader_line.contains("'0X"))
            && reader_line.ends_with("'\n")
        {
            // Remove the single quotes
            reader_line = reader_line.replace('\'', "");
            writer.write_all(reader_line.as_bytes())?;
        } else {
            writer.write_all(reader_line.as_bytes())?;
        }
        reader_line.clear();
    }
    Ok(())
}
