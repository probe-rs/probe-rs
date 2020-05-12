pub mod algorithm_binary;
pub mod fetch;
pub mod flash_device;
pub mod generate;
pub mod parser;

use std::{
    borrow::Cow,
    fs::{create_dir, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use probe_rs::config::{Chip, ChipFamily, FlashRegion, MemoryRegion, RamRegion};
use simplelog::*;
use structopt::StructOpt;

use parser::extract_flash_algo;

#[derive(StructOpt)]
enum TargetGen {
    /// Generate target description from ARM CMSIS-Packs
    Pack {
        #[structopt(
            name = "INPUT",
            parse(from_os_str),
            help = "A Pack file or the unziped Pack directory."
        )]
        input: PathBuf,
        #[structopt(
            name = "OUTPUT",
            parse(from_os_str),
            help = "An output directory where all the generated .yaml files are put in."
        )]
        output_dir: PathBuf,
    },
    /// Generates all the target descriptions from the entries listed in the ARM root VIDX/PIDX at https://www.keil.com/pack/Keil.pidx.
    Arm {
        #[structopt(
            name = "OUTPUT",
            parse(from_os_str),
            help = "An output directory where all the generated .yaml files are put in."
        )]
        output_dir: PathBuf,
    },
    /// Extract a flash algorithm from an ELF file
    Elf {
        /// ELF file containing a flash algorithm
        #[structopt(parse(from_os_str))]
        elf: PathBuf,
        /// Name of the extracted flash algorithm
        #[structopt(long = "name", short = "n")]
        name: Option<String>,
        /// Update an existing flash algorithm
        #[structopt(long = "update", short = "u", requires = "output")]
        update: bool,
        /// Output file, if provided, the generated target description will be written to this file.
        #[structopt(parse(from_os_str))]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let logger = TermLogger::init(LevelFilter::Info, Config::default(), TerminalMode::Mixed);
    if logger.is_err() {
        eprintln!("Logging backend could not be initialized.");
    }

    let options = TargetGen::from_args();

    match options {
        TargetGen::Pack { input, output_dir } => cmd_pack(&input, &output_dir)?,
        TargetGen::Elf {
            elf,
            output,
            update,
            name,
        } => cmd_elf(elf, output, update, name)?,
        TargetGen::Arm { output_dir } => cmd_arm(output_dir.as_path())?,
    }

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
        algorithm.name = Cow::Owned(name);
    }

    if update {
        // Update an existing target file

        let target_description_file = output.unwrap(); // Argument is checked by structopt, so we now its present.

        let target_description = File::open(&target_description_file).context(format!(
            "Unable to open target specification '{}'",
            target_description_file.display()
        ))?;

        let mut family = probe_rs::config::ChipFamily::from_yaml_reader(&target_description)?;

        // Close target description file, we want to overwrite it later
        drop(target_description);

        let algorithm_to_update = family
            .flash_algorithms
            .iter()
            .position(|old_algorithm| old_algorithm.name == algorithm.name);

        match algorithm_to_update {
            None => bail!("Unable to update flash algorithm in target description file '{}'. Did not find an existing algorithm with name '{}'", target_description_file.display(), &algorithm.name),
            Some(index) => family.flash_algorithms.to_mut()[index] = algorithm,
        }

        let target_description = File::create(&target_description_file)?;

        serde_yaml::to_writer(&target_description, &family)?;
    } else {
        // Create a complete target specification, with place holder values
        let algorithm_name = algorithm.name.clone();

        let chip_family = ChipFamily {
            name: Cow::Borrowed("<family name>"),
            manufacturer: None,
            variants: Cow::Owned(vec![Chip {
                part: None,
                name: Cow::Borrowed("<chip name>"),
                memory_map: Cow::Borrowed(&[
                    MemoryRegion::Flash(FlashRegion {
                        is_boot_memory: false,
                        range: 0..0x2000,
                    }),
                    MemoryRegion::Ram(RamRegion {
                        is_boot_memory: true,
                        range: 0x1_0000..0x2_0000,
                    }),
                ]),
                flash_algorithms: Cow::Owned(vec![algorithm_name]),
            }]),
            flash_algorithms: Cow::Owned(vec![algorithm]),
            core: Cow::Borrowed("<mcu core>"),
        };

        let serialized = serde_yaml::to_string(&chip_family)?;

        match output {
            Some(output) => {
                // Ensure we don't overwrite an existing file
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)
                    .context(format!(
                        "Failed to create target file '{}'.",
                        output.display()
                    ))?;

                file.write_all(serialized.as_bytes())?;
            }
            None => println!("{}", serialized),
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
        create_dir(&out_dir).context(format!(
            "Failed to create output directory '{}'.",
            out_dir.display()
        ))?;
    }

    let mut families = Vec::<ChipFamily>::new();

    if input.is_file() {
        generate::visit_file(&input, &mut families)
            .context(format!("Failed to process file {}.", input.display()))?;
    } else {
        // Look for the .pdsc file in the given dir and it's child directories.
        generate::visit_dirs(&input, &mut families)
            .context("Failed to generate target configuration.")?;

        // Check that we found at least a single .pdsc file
        ensure!(
            !families.is_empty(),
            "Unable to find any .pdsc files in the provided input directory."
        );
    }

    let mut generated_files = Vec::with_capacity(families.len());

    for family in &families {
        let path = out_dir.join(family.name.clone().into_owned() + ".yaml");
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create file '{}'.", path.display()))?;
        serde_yaml::to_writer(file, &family)?;

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
fn cmd_arm(out_dir: &Path) -> Result<()> {
    if !out_dir.exists() {
        create_dir(&out_dir).context(format!(
            "Failed to create output directory '{}'.",
            out_dir.display()
        ))?;
    }

    let mut families = Vec::<ChipFamily>::new();

    generate::visit_arm_files(&mut families)?;

    let mut generated_files = Vec::with_capacity(families.len());

    for family in &families {
        let path = out_dir.join(family.name.clone().into_owned() + ".yaml");
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create file '{}'.", path.display()))?;
        serde_yaml::to_writer(file, &family)?;

        generated_files.push(path);
    }

    println!("Generated {} target definition(s):", generated_files.len());

    for file in generated_files {
        println!("\t{}", file.display());
    }

    Ok(())
}
