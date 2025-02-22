use anyhow::{Context, Result, ensure};
use clap::Parser;
use probe_rs_target::ChipFamily;
use std::{
    env::current_dir,
    fs::create_dir,
    num::ParseIntError,
    path::{Path, PathBuf},
};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

use target_gen::{
    commands::{
        elf::{cmd_elf, serialize_to_yaml_string},
        test::cmd_test,
    },
    generate,
};

#[derive(clap::Parser)]
enum TargetGen {
    /// Generate target description from ARM CMSIS-Packs
    Pack {
        #[clap(
            value_name = "INPUT",
            value_parser,
            help = "A Pack file or the unziped Pack directory."
        )]
        input: PathBuf,
        #[clap(
            value_name = "OUTPUT",
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
        /// Specify a fixed load address for the flash algorithm
        ///
        /// If the flash algorithm should be loaded to a fixed address, this flag can be set.
        /// The load address will be read from the ELF file.
        #[clap(long)]
        fixed_load_address: bool,
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
    /// Generates a target yaml from a flash algorithm Rust project.
    ///
    /// Extracts parameters and functions from the ELF, generates the target yaml file
    /// and runs the flash algorithm on the given attached target.
    ///
    /// This can be used as a cargo runner.
    Test {
        /// The path of the template YAML definition file.
        /// This file plus the information of the ELF will be merged
        /// and stored into the `definition_export_path` file.
        template_path: PathBuf,
        /// The path of the completed YAML definition file.
        definition_export_path: PathBuf,
        /// The path to the ELF.
        target_artifact: PathBuf,
        /// The address used as the start of flash memory area to perform test.
        #[clap(long = "test-address", value_parser = parse_u64)]
        test_start_sector_address: Option<u64>,
        /// The name of the chip to use for the test, if there are multiple to choose from.
        #[clap(long = "chip")]
        chip: Option<String>,
        /// Name of the flash algorithm to test
        #[clap(long = "name", short = 'n')]
        name: Option<String>,
    },
    /// Loads and updates target description from YAML files.
    Reformat {
        /// The path of the YAML definition file or folder.
        yaml_path: PathBuf,
    },
}

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();

    let options = TargetGen::parse();

    let t = std::time::Instant::now();

    match options {
        TargetGen::Pack { input, output_dir } => cmd_pack(&input, &output_dir)?,
        TargetGen::Elf {
            elf,
            output,
            update,
            name,
            fixed_load_address,
        } => cmd_elf(
            elf.as_path(),
            fixed_load_address,
            output.as_deref(),
            update,
            name,
        )?,
        TargetGen::Arm {
            output_dir,
            pack_filter: chip_family,
            list,
        } => cmd_arm(output_dir, chip_family, list).await?,
        TargetGen::Test {
            target_artifact,
            template_path,
            definition_export_path,
            test_start_sector_address,
            chip,
            name,
        } => cmd_test(
            target_artifact.as_path(),
            template_path.as_path(),
            definition_export_path.as_path(),
            test_start_sector_address,
            chip,
            name,
        )?,
        TargetGen::Reformat { yaml_path } => {
            if yaml_path.is_dir() {
                let entries = std::fs::read_dir(&yaml_path).context(format!(
                    "Failed to read directory '{}'.",
                    yaml_path.display()
                ))?;

                for entry in entries {
                    let entry = entry.context("Failed to read directory entry.")?;
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "yaml") {
                        refresh_yaml(&path)?;
                    }
                }
            } else {
                refresh_yaml(&yaml_path)?;
            }
        }
    }

    println!("Finished in {:?}", t.elapsed());

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

    save_files(out_dir, &families)?;

    Ok(())
}

/// Handle the arm subcommand.
/// Generated target descriptions will be placed in `out_dir`.
async fn cmd_arm(out_dir: Option<PathBuf>, chip_family: Option<String>, list: bool) -> Result<()> {
    if list {
        let mut packs = target_gen::fetch::get_vidx().await?;
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

    generate::visit_arm_files(&mut families, chip_family).await?;

    save_files(&out_dir, &families)?;

    Ok(())
}

fn refresh_yaml(yaml_path: &Path) -> Result<()> {
    let yaml = std::fs::read_to_string(yaml_path)
        .context(format!("Failed to read file '{}'.", yaml_path.display()))?;

    let family = serde_yaml::from_str::<ChipFamily>(&yaml)
        .context(format!("Failed to parse file '{}'.", yaml_path.display()))?;

    let yaml = serialize_to_yaml_string(&family)?;

    std::fs::write(yaml_path, yaml)
        .context(format!("Failed to write file '{}'.", yaml_path.display()))?;

    Ok(())
}

fn save_files(out_dir: &Path, families: &[ChipFamily]) -> Result<()> {
    let mut generated_files = Vec::with_capacity(families.len());

    for family in families {
        let path = out_dir.join(family.name.clone().replace(' ', "_") + ".yaml");

        let yaml = serialize_to_yaml_string(family)?;
        std::fs::write(&path, yaml)
            .context(format!("Failed to create file '{}'.", path.display()))?;

        generated_files.push(path);
    }

    println!("Generated {} target definition(s):", generated_files.len());

    for file in generated_files {
        println!("\t{}", file.display());
    }

    Ok(())
}
