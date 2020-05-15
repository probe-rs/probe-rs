mod logging;

use structopt;

use anyhow::{anyhow, Context, Result};
use cargo_toml::Manifest;
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::{
    env,
    error::Error,
    fmt,
    io::Write,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::Arc,
    time::Instant,
};
use structopt::StructOpt;

use serde::Deserialize;

use probe_rs::{
    architecture::arm::ap::AccessPortError,
    config::TargetSelector,
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format, ProgressEvent},
    DebugProbeError, DebugProbeSelector, Probe, WireProtocol,
};

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(name = "chip", long = "chip")]
    chip: Option<String>,
    #[structopt(name = "chip description file path", long = "chip-description-path")]
    chip_description_path: Option<String>,
    // TODO: enable once the plugin architecture is here.
    // #[structopt(name = "nrf-recover", long = "nrf-recover")]
    // nrf_recover: bool,
    #[structopt(name = "list-chips", long = "list-chips")]
    list_chips: bool,
    #[structopt(
        name = "list-probes",
        long = "list-probes",
        help = "Lists all the connected probes that can be seen. If udev rules or permissions are wrong, some probes might not be listed."
    )]
    list_probes: bool,
    #[structopt(name = "disable-progressbars", long = "disable-progressbars")]
    disable_progressbars: bool,
    #[structopt(name = "protocol", long = "protocol", default_value = "swd")]
    protocol: WireProtocol,
    #[structopt(long = "probe-selector")]
    probe_selector: Option<DebugProbeSelector>,
    #[structopt(
        name = "gdb",
        long = "gdb",
        help = "Use this flag to automatically spawn a GDB server instance after flashing the target."
    )]
    gdb: bool,
    #[structopt(
        name = "no-download",
        long = "no-download",
        help = "Use this flag to prevent the actual flashing procedure (use if you just want to attach GDB)."
    )]
    no_download: bool,
    #[structopt(
        name = "reset-halt",
        long = "reset-halt",
        help = "Use this flag to reset and halt (instead of just a reset) the attached core after flashing the target."
    )]
    reset_halt: bool,
    #[structopt(
        name = "host:port",
        long = "gdb-connection-string",
        help = "Use this flag to override the default GDB connection string (localhost:1337)."
    )]
    gdb_connection_string: Option<String>,
    #[structopt(
        name = "level",
        long = "log",
        help = "Use this flag to set the log level. Default is `warning`. Possible choices are [error, warning, info, debug, trace]."
    )]
    log: Option<log::Level>,
    #[structopt(name = "speed", long = "speed", help = "The protocol speed in kHz.")]
    speed: Option<u32>,
    #[structopt(
        name = "restore-unwritten",
        long = "restore-unwritten",
        help = "Enable this flag to restore all bytes erased in the sector erase but not overwritten by any page."
    )]
    restore_unwritten: bool,
    #[structopt(
        name = "filename",
        long = "flash-layout",
        help = "Requests the flash builder to output the layout into the given file in SVG format."
    )]
    flash_layout_output_path: Option<String>,
    #[structopt(
        name = "elf file",
        long = "elf",
        help = "The path to the ELF file to be flashed."
    )]
    elf: Option<String>,
    #[structopt(
        name = "directory",
        long = "work-dir",
        help = "The work directory from which cargo-flash should operate from."
    )]
    work_dir: Option<String>,

    // `cargo build` arguments
    #[structopt(name = "binary", long = "bin")]
    bin: Option<String>,
    #[structopt(name = "example", long = "example")]
    example: Option<String>,
    #[structopt(name = "package", short = "p", long = "package")]
    package: Option<String>,
    #[structopt(name = "release", long = "release")]
    release: bool,
    #[structopt(name = "target", long = "target")]
    target: Option<String>,
    #[structopt(name = "PATH", long = "manifest-path", parse(from_os_str))]
    manifest_path: Option<PathBuf>,
    #[structopt(long)]
    no_default_features: bool,
    #[structopt(long)]
    all_features: bool,
    #[structopt(long)]
    features: Vec<String>,
}

const ARGUMENTS_TO_REMOVE: &[&str] = &[
    "chip=",
    "speed=",
    "restore-unwritten",
    "flash-layout=",
    "chip-description-path=",
    "list-chips",
    "list-probes",
    "probe-selector=",
    "elf=",
    "work-dir=",
    "disable-progressbars",
    "protocol=",
    "probe-index=",
    "gdb",
    "no-download",
    "reset-halt",
    "gdb-connection-string=",
    "nrf-recover",
    "log=",
];

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Meta {
    pub chip: Option<String>,
}

fn main() {
    match main_try() {
        Ok(_) => (),
        Err(e) => {
            // Ensure stderr is flushed before calling proces::exit,
            // otherwise the process might panic, because it tries
            // to access stderr during shutdown.
            //
            // We ignore the errors, not much we can do anyway.
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "       {} {:?}", "Error".red().bold(), e);
            let _ = stderr.flush();

            process::exit(1);
        }
    }
}

fn main_try() -> Result<()> {
    let mut args = std::env::args();

    // When called by Cargo, the first argument after the binary name will be `flash`. If that's the
    // case, remove one argument (`Opt::from_iter` will remove the binary name by itself).
    if env::args().nth(1) == Some("flash".to_string()) {
        args.next();
    }

    let mut args: Vec<_> = args.collect();

    // Get commandline options.
    let opt = Opt::from_iter(&args);

    logging::init(opt.log);

    // Load cargo manifest if available and parse out meta object
    let meta = match Manifest::<Meta>::from_path_with_metadata("Cargo.toml") {
        Ok(m) => m.package.map(|p| p.metadata).flatten(),
        Err(_e) => None,
    };

    // If someone wants to list the connected probes, just do that and exit.
    if opt.list_probes {
        list_connected_devices()?;
        std::process::exit(0);
    }

    // Make sure we load the config given in the cli parameters.
    if let Some(cdp) = opt.chip_description_path {
        probe_rs::config::registry::add_target_from_yaml(&Path::new(&cdp))?;
    }

    let chip = if opt.list_chips {
        print_families()?;
        std::process::exit(0);
    } else {
        // First use command line, then manifest, then default to auto
        match (opt.chip, meta.map(|m| m.chip).flatten()) {
            (Some(c), _) => c.into(),
            (_, Some(c)) => c.into(),
            _ => TargetSelector::Auto,
        }
    };

    args.remove(0); // Remove executable name

    // Remove all arguments that `cargo build` does not understand.
    remove_arguments(ARGUMENTS_TO_REMOVE, &mut args);

    // Change the work dir if the user asked to do so
    if let Some(work_dir) = opt.work_dir {
        std::env::set_current_dir(work_dir)?;
    }

    let path: PathBuf = if let Some(path) = opt.elf {
        path.into()
    } else {
        let status = Command::new("cargo")
            .arg("build")
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?
            .wait()?;

        if !status.success() {
            handle_failed_command(status)
        }

        // Try and get the cargo project information.
        let project = cargo_project::Project::query(".")
            .map_err(|e| anyhow!("failed to parse Cargo project information: {}", e))?;

        // Decide what artifact to use.
        let artifact = if let Some(bin) = &opt.bin {
            cargo_project::Artifact::Bin(bin)
        } else if let Some(example) = &opt.example {
            cargo_project::Artifact::Example(example)
        } else {
            cargo_project::Artifact::Bin(project.name())
        };

        // Decide what profile to use.
        let profile = if opt.release {
            cargo_project::Profile::Release
        } else {
            cargo_project::Profile::Dev
        };

        // Try and get the artifact path.
        project
            .path(
                artifact,
                profile,
                opt.target.as_ref().map(|t| &**t),
                "x86_64-unknown-linux-gnu",
            )
            .map_err(|e| anyhow!("Couldn't get artifact path: {}", e))?
    };

    logging::println(format!(
        "    {} {}",
        "Flashing".green().bold(),
        path.display()
    ));

    let list = Probe::list_all();

    // If we got a probe selector as an argument, open the probe matching the selector if possible.
    let mut probe = match opt.probe_selector {
        Some(selector) => Probe::open(selector)?,
        None => {
            // Only automatically select a probe if there is only
            // a single probe detected.
            if list.len() > 1 {
                return Err(anyhow!("More than a single probe detected. Use the --probe-selector argument to select which probe to use."));
            }

            Probe::open(
                list.first()
                    .ok_or_else(|| anyhow!("No supported probe was found"))?,
            )?
        }
    };

    // Select the protocol if any was passed as an argument.
    probe.select_protocol(opt.protocol)?;

    // Disabled for now
    // TODO: reenable once we got the plugin architecture working.
    // if opt.nrf_recover {
    //     match device.probe_type {
    //         DebugProbeType::DAPLink => {
    //             probe.nrf_recover()?;
    //         }
    //         DebugProbeType::STLink => {
    //             return Err(format_err!("It isn't possible to recover with a ST-Link"));
    //         }
    //     };
    // }

    let protocol_speed = if let Some(speed) = opt.speed {
        let actual_speed = probe.set_speed(speed)?;

        if actual_speed < speed {
            log::warn!(
                "Unable to use specified speed of {} kHz, actual speed used is {} kHz",
                speed,
                actual_speed
            );
        }

        actual_speed
    } else {
        probe.speed_khz()
    };

    log::info!("Protocol speed {} kHz", protocol_speed);

    let mut session = probe.attach(chip)?;

    // Start timer.
    let instant = Instant::now();

    if !opt.no_download {
        if !opt.disable_progressbars {
            // Create progress bars.
            let multi_progress = MultiProgress::new();
            let style = ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("##-")
                    .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})");

            // Create a new progress bar for the fill progress if filling is enabled.
            let fill_progress = if opt.restore_unwritten {
                let fill_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
                fill_progress.set_style(style.clone());
                fill_progress.set_message("     Reading flash  ");
                Some(fill_progress)
            } else {
                None
            };

            // Create a new progress bar for the erase progress.
            let erase_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
            {
                logging::set_progress_bar(erase_progress.clone());
            }
            erase_progress.set_style(style.clone());
            erase_progress.set_message("     Erasing sectors");

            // Create a new progress bar for the program progress.
            let program_progress = multi_progress.add(ProgressBar::new(0));
            program_progress.set_style(style);
            program_progress.set_message(" Programming pages  ");

            // Register callback to update the progress.
            let flash_layout_output_path = opt.flash_layout_output_path.clone();
            let progress = FlashProgress::new(move |event| {
                use ProgressEvent::*;
                match event {
                    Initialized { flash_layout } => {
                        let total_page_size: u32 =
                            flash_layout.pages().iter().map(|s| s.size()).sum();
                        let total_sector_size: u32 =
                            flash_layout.sectors().iter().map(|s| s.size()).sum();
                        let total_fill_size: u32 =
                            flash_layout.fills().iter().map(|s| s.size()).sum();
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.set_length(total_fill_size as u64)
                        }
                        erase_progress.set_length(total_sector_size as u64);
                        program_progress.set_length(total_page_size as u64);
                        let visualizer = flash_layout.visualize();
                        flash_layout_output_path
                            .as_ref()
                            .map(|path| visualizer.write_svg(path));
                    }
                    StartedFlashing => {
                        program_progress.enable_steady_tick(100);
                        program_progress.reset_elapsed();
                    }
                    StartedErasing => {
                        erase_progress.enable_steady_tick(100);
                        erase_progress.reset_elapsed();
                    }
                    StartedFilling => {
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.enable_steady_tick(100)
                        };
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.reset_elapsed()
                        };
                    }
                    PageFlashed { size, .. } => {
                        program_progress.inc(size as u64);
                    }
                    SectorErased { size, .. } => {
                        erase_progress.inc(size as u64);
                    }
                    PageFilled { size, .. } => {
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.inc(size as u64)
                        };
                    }
                    FailedErasing => {
                        erase_progress.abandon();
                        program_progress.abandon();
                    }
                    FinishedErasing => {
                        erase_progress.finish();
                    }
                    FailedProgramming => {
                        program_progress.abandon();
                    }
                    FinishedProgramming => {
                        program_progress.finish();
                    }
                    FailedFilling => {
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.abandon()
                        };
                    }
                    FinishedFilling => {
                        if let Some(fp) = fill_progress.as_ref() {
                            fp.finish()
                        };
                    }
                }
            });

            // Make the multi progresses print.
            // indicatif requires this in a separate thread as this join is a blocking op,
            // but is required for printing multiprogress.
            let progress_thread_handle = std::thread::spawn(move || {
                multi_progress.join().unwrap();
            });

            download_file_with_options(
                &mut session,
                path.as_path(),
                Format::Elf,
                DownloadOptions {
                    progress: Some(&progress),
                    keep_unwritten_bytes: opt.restore_unwritten,
                },
            )
            .with_context(|| format!("failed to flash {}", path.display()))?;

            // We don't care if we cannot join this thread.
            let _ = progress_thread_handle.join();
        } else {
            download_file_with_options(
                &mut session,
                path.as_path(),
                Format::Elf,
                DownloadOptions {
                    progress: None,
                    keep_unwritten_bytes: opt.restore_unwritten,
                },
            )
            .with_context(|| format!("failed to flash {}", path.display()))?;
        }

        // Stop timer.
        let elapsed = instant.elapsed();
        logging::println(format!(
            "    {} in {}s",
            "Finished".green().bold(),
            elapsed.as_millis() as f32 / 1000.0,
        ));
    }

    {
        let mut core = session.core(0)?;
        if opt.reset_halt {
            core.reset_and_halt()?;
        } else {
            core.reset()?;
        }
    }

    if opt.gdb {
        let gdb_connection_string = opt
            .gdb_connection_string
            .or_else(|| Some("localhost:1337".to_string()));
        // This next unwrap will always resolve as the connection string is always Some(T).
        logging::println(format!(
            "Firing up GDB stub at {}",
            gdb_connection_string.as_ref().unwrap(),
        ));
        if let Err(e) = probe_rs_gdb_server::run(gdb_connection_string, session) {
            logging::eprintln("During the execution of GDB an error was encountered:");
            logging::eprintln(format!("{:?}", e));
        }
    }

    Ok(())
}

fn print_families() -> Result<()> {
    logging::println("Available chips:");
    for family in probe_rs::config::registry::families()
        .with_context(|| format!("Families could not be read"))?
    {
        logging::println(&family.name);
        logging::println("    Variants:");
        for variant in family.variants() {
            logging::println(format!("        {}", variant.name));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn handle_failed_command(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    let status = status.code().or_else(|| status.signal()).unwrap_or(1);
    std::process::exit(status)
}

#[cfg(not(unix))]
fn handle_failed_command(status: std::process::ExitStatus) -> ! {
    let status = status.code().unwrap_or(1);
    std::process::exit(status)
}

#[derive(Debug)]
pub enum DownloadError {
    DebugProbe(DebugProbeError),
    AccessPort(AccessPortError),
    StdIO(std::io::Error),
    Quit,
}

impl Error for DownloadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use crate::DownloadError::*;

        match self {
            DebugProbe(ref e) => Some(e),
            AccessPort(ref e) => Some(e),
            StdIO(ref e) => Some(e),
            Quit => None,
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use crate::DownloadError::*;

        match self {
            DebugProbe(ref e) => e.fmt(f),
            AccessPort(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            Quit => write!(f, "Quit error..."),
        }
    }
}

impl From<AccessPortError> for DownloadError {
    fn from(error: AccessPortError) -> Self {
        DownloadError::AccessPort(error)
    }
}

impl From<DebugProbeError> for DownloadError {
    fn from(error: DebugProbeError) -> Self {
        DownloadError::DebugProbe(error)
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        DownloadError::StdIO(error)
    }
}

/// Removes all arguments from the commandline input that `cargo build` does not understand.
/// All the arguments are removed in place!
/// It expects a list of arguments to be removed. If the argument can have a value it MUST contain a `=` at the end.
/// E.g:
/// ```rust
/// let arguments_to_remove = [
///     "foo", // Can be "--foo"
///     "bar=", // Can be "--bar=value" and "--bar value"
/// ];
/// ```
fn remove_arguments(arguments_to_remove: &[&'static str], arguments: &mut Vec<String>) {
    // We iterate all arguments that possibly have to be removed
    // and remove them if they occur to be in the input.
    for argument in arguments_to_remove {
        // Make sure the compared against arg does not contain an equal sign.
        // If the original arg contained an equal sign we take this as a hint
        // that the arg can be used as `--arg value` as well as `--arg=value`.
        // In the prior case we need to remove two arguments. So remember this.
        let (remove_two, clean_argument) = if argument.ends_with('=') {
            (true, format!("--{}", &argument[..argument.len() - 1]))
        } else {
            (false, format!("--{}", argument))
        };

        // Iterate all args in the input and if we find one that matches, we remove it.
        if let Some(index) = arguments
            .iter()
            .position(|x| x.starts_with(&format!("--{}", argument)))
        {
            // We remove the argument we found.
            arguments.remove(index);
        }

        // If the argument requires a value we also need to check for the case where no
        // = (equal sign) was present, in which case the value is a second argument
        // which we need to remove as well.
        if remove_two {
            // Iterate all args in the input and if we find one that matches, we remove it.
            if let Some(index) = arguments
                .iter()
                .position(|x| x.starts_with(&clean_argument))
            {
                // We remove the argument we found plus its value.
                arguments.remove(index);
                arguments.remove(index);
            }
        }
    }
}

#[test]
fn remove_arguments_test() {
    let arguments_to_remove = [
        "chip=",
        "chip-description-path=",
        "list-chips",
        "disable-progressbars",
        "protocol=",
        "probe-index=",
        "gdb",
        "no-download",
        "reset-halt",
        "gdb-connection-string=",
        "nrf-recover",
    ];

    let mut arguments = vec![
        "--chip=kek".to_string(),
        "--chip".to_string(),
        "kek".to_string(),
        "--chip-description-path=kek".to_string(),
        "--chip-description-path".to_string(),
        "kek".to_string(),
        "--list-chips".to_string(),
        "--disable-progressbars".to_string(),
        "--protocol=kek".to_string(),
        "--protocol".to_string(),
        "kek".to_string(),
        "--probe-index=kek".to_string(),
        "--probe-index".to_string(),
        "kek".to_string(),
        "--gdb".to_string(),
        "--no-download".to_string(),
        "--reset-halt".to_string(),
        "--gdb-connection-string=kek".to_string(),
        "--gdb-connection-string".to_string(),
        "kek".to_string(),
        "--nrf-recover".to_string(),
    ];

    remove_arguments(&arguments_to_remove, &mut arguments);

    assert!(arguments.len() == 0);
}

/// Lists all connected devices
fn list_connected_devices() -> Result<(), DebugProbeError> {
    let links = Probe::list_all();

    if !links.is_empty() {
        println!("The following devices were found:");
        links
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!("[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }

    Ok(())
}
