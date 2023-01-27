pub mod algorithm_binary;
pub mod commands;
pub mod fetch;
pub mod flash_device;
pub mod generate;
pub mod parser;

use anyhow::{ensure, Context, Result};
use clap::Parser;
use probe_rs::config::ChipFamily;
use std::{
    env::current_dir,
    fs::create_dir,
    path::{Path, PathBuf},
};
use tracing_subscriber::EnvFilter;

use crate::commands::{
    elf::{cmd_elf, serialize_to_yaml_file},
    export::cmd_export,
    run::cmd_run,
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
    /// It builds the project, extracts parameters and functions from the ELF and generates
    /// the target yaml file.
    Export {
        /// The path of the template YAML definition file.
        /// This file plus the information of the ELF will be merged
        /// and stored into the `definition_export_path` file.
        template_path: PathBuf,
        /// The path of the completed YAML definition file.
        definition_export_path: PathBuf,
        /// The path to the ELF.
        target_artifact: PathBuf,
    },
    /// Generates a target yaml from a flash algorithm Rust project.
    ///
    /// It builds the project, extracts parameters and functions from the ELF and generates
    /// the target yaml file and runs the flash algorithm on the given attached target.
    ///
    /// Works like `target-gen export` but also runs the flash algorithm.
    Run {
        /// The path of the template YAML definition file.
        /// This file plus the information of the ELF will be merged
        /// and stored into the `definition_export_path` file.
        template_path: PathBuf,
        /// The path of the completed YAML definition file.
        definition_export_path: PathBuf,
        /// The path to the ELF.
        target_artifact: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(EnvFilter::from_default_env())
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
        } => cmd_arm(output_dir, chip_family, list)?,
        TargetGen::Export {
            target_artifact,
            template_path,
            definition_export_path,
        } => cmd_export(
            target_artifact.as_path(),
            template_path.as_path(),
            definition_export_path.as_path(),
        )?,
        TargetGen::Run {
            target_artifact,
            template_path,
            definition_export_path,
        } => cmd_run(
            target_artifact.as_path(),
            template_path.as_path(),
            definition_export_path.as_path(),
        )?,
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
