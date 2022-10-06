use std::path::PathBuf;

use clap::{ArgAction, Parser};
use defmt_decoder::DEFMT_VERSION;
use git_version::git_version;
use log::Level;
use probe_rs::Probe;

use crate::probe;

/// Successfull termination of process.
const EXIT_SUCCESS: i32 = 0;

/// A Cargo runner for microcontrollers.
#[derive(Parser)]
#[command()]
pub struct Opts {
    /// Disable or enable backtrace (auto in case of panic or stack overflow).
    #[arg(long, default_value = "auto")]
    pub backtrace: String,

    /// Configure the number of lines to print before a backtrace gets cut off.
    #[arg(long, default_value = "50")]
    pub backtrace_limit: u32,

    /// The chip to program.
    #[arg(long, required = true, conflicts_with_all = HELPER_CMDS, env = "PROBE_RUN_CHIP")]
    chip: Option<String>,

    /// Path to chip description file, in YAML format.
    #[arg(long)]
    pub chip_description_path: Option<PathBuf>,

    /// Connect to device when NRST is pressed.
    #[arg(long)]
    pub connect_under_reset: bool,

    /// Disable use of double buffering while downloading flash.
    #[arg(long)]
    pub disable_double_buffering: bool,

    /// Path to an ELF firmware file.
    #[arg(required = true, conflicts_with_all = HELPER_CMDS)]
    elf: Option<PathBuf>,

    /// Output logs a structured json.
    #[arg(long)]
    pub json: bool,

    /// List supported chips and exit.
    #[arg(long)]
    list_chips: bool,

    /// Lists all the connected probes and exit.
    #[arg(long)]
    list_probes: bool,

    /// Whether to measure the program's stack consumption.
    #[arg(long)]
    pub measure_stack: bool,

    /// Skip writing the application binary to flash.
    #[arg(
        long,
        conflicts_with = "disable_double_buffering",
        conflicts_with = "verify"
    )]
    pub no_flash: bool,

    /// The probe to use (eg. `VID:PID`, `VID:PID:Serial`, or just `Serial`).
    #[arg(long, env = "PROBE_RUN_PROBE")]
    pub probe: Option<String>,

    /// Whether to shorten paths (e.g. to crates.io dependencies) in backtraces and defmt logs
    #[arg(long)]
    pub shorten_paths: bool,

    /// The probe clock frequency in kHz
    #[arg(long, env = "PROBE_RUN_SPEED")]
    pub speed: Option<u32>,

    /// Enable more verbose output.
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    /// Verifies the written program.
    #[arg(long)]
    pub verify: bool,

    /// Prints version information
    #[arg(short = 'V', long)]
    version: bool,

    /// Arguments passed after the ELF file path are discarded
    #[arg(allow_hyphen_values = true, hide = true, trailing_var_arg = true)]
    _rest: Vec<String>,
}

/// Helper commands, which will not execute probe-run normally.
const HELPER_CMDS: [&str; 3] = ["list_chips", "list_probes", "version"];

pub fn handle_arguments() -> anyhow::Result<i32> {
    let opts = Opts::parse();
    let verbose = opts.verbose;

    defmt_decoder::log::init_logger(verbose >= 1, opts.json, move |metadata| {
        if defmt_decoder::log::is_defmt_frame(metadata) {
            true // We want to display *all* defmt frames.
        } else {
            // Log depending on how often the `--verbose` (`-v`) cli-param is supplied:
            //   * 0: log everything from probe-run, with level "info" or higher
            //   * 1: log everything from probe-run
            //   * 2 or more: log everything
            match verbose {
                0 => metadata.target().starts_with("probe_run") && metadata.level() <= Level::Info,
                1 => metadata.target().starts_with("probe_run"),
                _ => true,
            }
        }
    });

    if opts.version {
        print_version();
        Ok(EXIT_SUCCESS)
    } else if opts.list_probes {
        probe::print(&Probe::list_all());
        Ok(EXIT_SUCCESS)
    } else if opts.list_chips {
        print_chips();
        Ok(EXIT_SUCCESS)
    } else if let (Some(elf), Some(chip)) = (opts.elf.as_deref(), opts.chip.as_deref()) {
        crate::run_target_program(elf, chip, &opts)
    } else {
        unreachable!("due to `StructOpt` constraints")
    }
}

fn print_chips() {
    let registry = probe_rs::config::families().expect("Could not retrieve chip family registry");
    for chip_family in registry {
        println!("{}\n    Variants:", chip_family.name);
        for variant in chip_family.variants.iter() {
            println!("        {}", variant.name);
        }
    }
}

/// The string reported by the `--version` flag
fn print_version() {
    /// Version from `Cargo.toml` e.g. `"0.1.4"`
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    /// `""` OR git hash e.g. `"34019f8"`
    ///
    /// `git describe`-docs:
    /// > The command finds the most recent tag that is reachable from a commit. (...)
    /// It suffixes the tag name with the number of additional commits on top of the tagged object
    /// and the abbreviated object name of the most recent commit.
    //
    // The `fallback` is `"--"`, cause this will result in "" after `fn extract_git_hash`.
    const GIT_DESCRIBE: &str = git_version!(fallback = "--", args = ["--long"]);
    // Extract the "abbreviated object name"
    let hash = extract_git_hash(GIT_DESCRIBE);

    println!(
        "{} {}\nsupported defmt version: {}",
        VERSION, hash, DEFMT_VERSION
    );
}

/// Extract git hash from a `git describe` statement
fn extract_git_hash(git_describe: &str) -> &str {
    git_describe.split('-').nth(2).unwrap()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::normal("v0.2.3-12-g25c50d2", "g25c50d2")]
    #[case::modified("v0.2.3-12-g25c50d2-modified", "g25c50d2")]
    #[case::fallback("--", "")]
    fn should_extract_hash_from_description(#[case] description: &str, #[case] expected: &str) {
        let hash = extract_git_hash(description);
        assert_eq!(hash, expected)
    }
}
