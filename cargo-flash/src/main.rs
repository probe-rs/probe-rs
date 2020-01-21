use structopt;

use colored::*;
use failure::format_err;
use std::{
    env,
    error::Error,
    fmt,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};
use structopt::StructOpt;

use probe_rs::{
    architecture::arm::ap::AccessPortError,
    config::registry::{Registry, SelectionStrategy},
    flash::download::{download_file, download_file_with_progress_reporting, Format},
    flash::{FlashProgress, ProgressEvent},
    DebugProbeError, Probe,
};

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(name = "chip", long = "chip")]
    chip: Option<String>,
    #[structopt(
        name = "chip description file path",
        short = "c",
        long = "chip-description-path"
    )]
    chip_description_path: Option<String>,
    #[structopt(name = "nrf-recover", long = "nrf-recover")]
    nrf_recover: bool,
    #[structopt(name = "list-chips", long = "list-chips")]
    list_chips: bool,
    #[structopt(name = "disable-progressbars", long = "disable-progressbars")]
    disable_progressbars: bool,

    /// The number associated with the debug probe to use
    #[structopt(long = "probe-index")]
    n: Option<usize>,
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
        name = "gdb-connection-string",
        long = "gdb-connection-string",
        help = "Use this flag to override the default GDB connection string (localhost:1337)."
    )]
    gdb_connection_string: Option<String>,

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

fn main() {
    pretty_env_logger::init();
    match main_try() {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{}: {}", "error".red().bold(), e);
            process::exit(1);
        }
    }
}

fn main_try() -> Result<(), failure::Error> {
    let mut args = std::env::args();

    // When called by Cargo, the first argument after the binary name will be `flash`. If that's the
    // case, remove one argument (`Opt::from_iter` will remove the binary name by itself).
    if env::args().nth(1) == Some("flash".to_string()) {
        args.next();
    }

    let mut args: Vec<_> = args.collect();

    // Get commandline options.
    let opt = Opt::from_iter(&args);

    if opt.list_chips {
        print_families();
        std::process::exit(0);
    }

    args.remove(0); // Remove executable name

    // Remove possible `--chip <chip>` arguments as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| *x == "--chip") {
        args.remove(index);
        args.remove(index);
    }

    // Remove possible `--chip=<chip>` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--chip=")) {
        args.remove(index);
    }

    // Remove possible `--chip-description-path <chip description path>` arguments as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| *x == "--chip-description-path") {
        args.remove(index);
        args.remove(index);
    }

    // Remove possible `--chip-description-path=<chip description path>` arguments as cargo build does not understand it.
    if let Some(index) = args
        .iter()
        .position(|x| x.starts_with("--chip-description-path="))
    {
        args.remove(index);
    }

    // Remove possible `-c <chip description path>` arguments as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| *x == "-c") {
        args.remove(index);
        args.remove(index);
    }

    // Remove possible `-c=<chip description path>` arguments as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("-c=")) {
        args.remove(index);
    }

    // Remove possible `--disable-progressbars` argument as cargo build does not understand it.
    if let Some(index) = args
        .iter()
        .position(|x| x.starts_with("--disable-progressbars"))
    {
        args.remove(index);
    }

    // Remove possible `--nrf-recover` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--nrf-recover")) {
        args.remove(index);
    }

    // Remove possible `--gdb` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--gdb")) {
        args.remove(index);
    }

    // Remove possible `--no-download` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--no-download")) {
        args.remove(index);
    }

    // Remove possible `--reset-halt` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--reset-halt")) {
        args.remove(index);
    }

    // Remove possible `--gdb-connection-string` argument as cargo build does not understand it.
    if let Some(index) = args
        .iter()
        .position(|x| x.starts_with("--gdb-connection-string"))
    {
        args.remove(index);
    }

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
        .map_err(|e| format_err!("failed to parse Cargo project information: {}", e))?;

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
    let path = project.path(
        artifact,
        profile,
        opt.target.as_ref().map(|t| &**t),
        "x86_64-unknown-linux-gnu",
    )?;

    let path_str = match path.to_str() {
        Some(s) => s,
        None => panic!(),
    };

    println!("    {} {}", "Flashing".green().bold(), path_str);

    let list = Probe::list_all();

    let device = match opt.n {
        Some(index) => list.get(index).ok_or_else(|| {
            format_err!("Unable to open probe with index {}: Probe not found", index)
        })?,
        None => {
            // Only automatically select a probe if there is only
            // a single probe detected.
            if list.len() > 1 {
                return Err(format_err!("More than a single probe detected. Use the --probe-index argument to select which probe to use."));
            }

            list.first()
                .ok_or_else(|| format_err!("no supported probe was found"))?
        }
    };

    let mut probe = Probe::from_probe_info(&device)?;

    if opt.nrf_recover {
        // TODO:
        // match device.probe_type {
        //     DebugProbeType::DAPLink => {
        //         probe.nrf_recover()?;
        //     }
        //     DebugProbeType::STLink => {
        //         return Err(format_err!("It isn't possible to recover with a ST-Link"));
        //     }
        // };
        eprintln!("The nrf-recover option is currently disabled for stability reasons.");
        std::process::exit(1);
    }

    let strategy = if let Some(identifier) = opt.chip.clone() {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        eprintln!("Autodetection of the target is currently disabled for stability reasons.");
        std::process::exit(1);
        // TODO:
        // SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let mut registry = Registry::from_builtin_families();
    if let Some(cdp) = opt.chip_description_path {
        registry.add_target_from_yaml(&Path::new(&cdp))?;
    }

    let target = registry.get_target(strategy)?;
    let session = probe.attach(target, None)?;

    // Start timer.
    let instant = Instant::now();

    let mm = session.target.memory_map.clone();

    if !opt.no_download {
        if !opt.disable_progressbars {
            // Create progress bars.
            let multi_progress = indicatif::MultiProgress::new(); //with_draw_target(indicatif::ProgressDrawTarget::stdout_nohz());
            let style = indicatif::ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("##-")
                    .template("    {msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})");

            // Create a new progress bar for the erase progress.
            let erase_progress = multi_progress.add(indicatif::ProgressBar::new(0));
            erase_progress.set_style(style.clone());
            erase_progress.set_message("Erasing sectors  ");

            // Create a new progress bar for the program progress.
            let program_progress = multi_progress.add(indicatif::ProgressBar::new(0));
            program_progress.set_style(style);
            program_progress.set_message("Programming pages");

            // Register callback to update the progress.
            let progress = FlashProgress::new(move |event| {
                use ProgressEvent::*;
                match event {
                    Initialized {
                        total_pages,
                        total_sector_size,
                        page_size,
                    } => {
                        erase_progress.set_length(total_sector_size as u64);
                        program_progress.set_length(total_pages as u64 * page_size as u64);
                    }
                    StartedFlashing => {
                        program_progress.enable_steady_tick(100);
                        program_progress.reset_elapsed();
                    }
                    StartedErasing => {
                        erase_progress.enable_steady_tick(100);
                        erase_progress.reset_elapsed();
                    }
                    PageFlashed { size, .. } => {
                        program_progress.inc(size as u64);
                    }
                    SectorErased { size, .. } => {
                        erase_progress.inc(size as u64);
                    }
                    FinishedErasing => {
                        erase_progress.finish();
                    }
                    FinishedProgramming => {
                        program_progress.finish();
                    }
                }
            });

            // Make the multi progresses print.
            // indicatif requires this in a separate thread as this join is a blocking op,
            // but is required for printing multiprogress.
            let progress_thread_handle = std::thread::spawn(move || {
                multi_progress.join().unwrap();
            });

            download_file_with_progress_reporting(
                session.clone(),
                std::path::Path::new(&path_str.to_string().as_str()),
                Format::Elf,
                &mm,
                &progress,
            )
            .map_err(|e| format_err!("failed to flash {}: {}", path_str, e))?;

            // We don't care if we cannot join this thread.
            let _ = progress_thread_handle.join();
        } else {
            download_file(
                session.clone(),
                std::path::Path::new(&path_str.to_string().as_str()),
                Format::Elf,
                &mm,
            )
            .map_err(|e| format_err!("failed to flash {}: {}", path_str, e))?;
        }

        // Stop timer.
        let elapsed = instant.elapsed();
        println!(
            "    {} in {}s",
            "Finished".green().bold(),
            elapsed.as_millis() as f32 / 1000.0
        );
    }

    if opt.reset_halt {
        session.attach_to_core(0)?.reset_and_halt()?;
    } else {
        session.attach_to_core(0)?.reset()?;
    }

    if opt.gdb {
        let gdb_connection_string = opt
            .gdb_connection_string
            .or_else(|| Some("localhost:1337".to_string()));
        // This next unwrap will always resolve as the connection string is always Some(T).
        println!(
            "Firing up GDB stub at {}",
            gdb_connection_string.as_ref().unwrap()
        );
        if let Err(e) =
            probe_rs_gdb_server::run(gdb_connection_string, Arc::new(Mutex::new(session)))
        {
            eprintln!("During the execution of GDB an error was encountered:");
            eprintln!("{:?}", e);
        }
    }

    Ok(())
}

fn print_families() {
    println!("Available chips:");
    let registry = Registry::from_builtin_families();
    for family in registry.families() {
        println!("{}", family.name);
        println!("    Variants:");
        for variant in family.variants() {
            println!("        {}", variant.name);
        }

        println!("    Algorithms:");
        for (name, algorithm) in family.algorithms() {
            println!("        {} ({})", name, algorithm.description);
        }
    }
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
