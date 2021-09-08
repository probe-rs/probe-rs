use std::path::PathBuf;

use defmt_decoder::DEFMT_VERSION;
use git_version::git_version;
use log::Level;
use probe_rs::Probe;
use structopt::{clap::AppSettings, StructOpt};

use crate::probe;

/// Successfull termination of process.
const EXIT_SUCCESS: i32 = 0;

/// A Cargo runner for microcontrollers.
#[derive(StructOpt)]
#[structopt(name = "probe-run", setting = AppSettings::TrailingVarArg)]
pub(crate) struct Opts {
    /// List supported chips and exit.
    #[structopt(long)]
    list_chips: bool,

    /// Lists all the connected probes and exit.
    #[structopt(long)]
    list_probes: bool,

    /// The chip to program.
    #[structopt(long, required_unless_one(&["list-chips", "list-probes", "version"]), env = "PROBE_RUN_CHIP")]
    chip: Option<String>,

    /// The probe to use (eg. `VID:PID`, `VID:PID:Serial`, or just `Serial`).
    #[structopt(long, env = "PROBE_RUN_PROBE")]
    pub(crate) probe: Option<String>,

    /// The probe clock frequency in kHz
    #[structopt(long)]
    pub(crate) speed: Option<u32>,

    /// Path to an ELF firmware file.
    #[structopt(name = "ELF", parse(from_os_str), required_unless_one(&["list-chips", "list-probes", "version"]))]
    elf: Option<PathBuf>,

    /// Skip writing the application binary to flash.
    #[structopt(long, conflicts_with = "defmt")]
    pub(crate) no_flash: bool,

    /// Connect to device when NRST is pressed.
    #[structopt(long)]
    pub(crate) connect_under_reset: bool,

    /// Enable more verbose logging.
    #[structopt(short, long, parse(from_occurrences))]
    verbose: u32,

    /// Prints version information
    #[structopt(short = "V", long)]
    version: bool,

    /// Disable or enable backtrace (auto in case of panic or stack overflow).
    #[structopt(long, default_value = "auto")]
    pub(crate) backtrace: String,

    /// Configure the number of lines to print before a backtrace gets cut off
    #[structopt(long, default_value = "50")]
    pub(crate) backtrace_limit: u32,

    /// Whether to shorten paths (e.g. to crates.io dependencies) in backtraces and defmt logs
    #[structopt(long)]
    pub(crate) shorten_paths: bool,

    /// Whether to measure the program's stack consumption.
    #[structopt(long)]
    pub(crate) measure_stack: bool,

    /// Arguments passed after the ELF file path are discarded
    #[structopt(name = "REST")]
    _rest: Vec<String>,
}

pub(crate) fn handle_arguments() -> anyhow::Result<i32> {
    let opts: Opts = Opts::from_args();
    let verbose = opts.verbose;

    defmt_decoder::log::init_logger(verbose >= 1, move |metadata| {
        if defmt_decoder::log::is_defmt_frame(metadata) {
            true // We want to display *all* defmt frames.
        } else {
            // Log depending on how often the `--verbose` (`-v`) cli-param is supplied:
            //   * 0: log everything from probe-run, with level "info" or higher
            //   * 1: log everything from probe-run
            //   * 2 or more: log everything
            if verbose >= 2 {
                true
            } else if verbose >= 1 {
                metadata.target().starts_with("probe_run")
            } else {
                metadata.target().starts_with("probe_run") && metadata.level() <= Level::Info
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
