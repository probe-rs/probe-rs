extern crate structopt;

use colored::*;
use failure::format_err;
use std::{
    env, error, fmt,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    time::Instant,
};
use structopt::StructOpt;

use probe_rs::{
    config::registry::{Registry, SelectionStrategy},
    flash::download::{download_file, Format},
    probe::{daplink, stlink, DebugProbe, DebugProbeType, MasterProbe, WireProtocol},
    target::info::ChipInfo,
    Error, Session,
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

    // Remove possible `--nrf-recover` argument as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| x.starts_with("--nrf-recover")) {
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

    let mut list = daplink::tools::list_daplink_devices();
    list.extend(stlink::tools::list_stlink_devices());

    let device = list
        .pop()
        .ok_or_else(|| format_err!("no supported probe was found"))?;

    let mut probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = daplink::DAPLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            let mut probe = MasterProbe::from_specific_probe(link);
            if opt.nrf_recover {
                probe.nrf_recover()?;
            }
            probe
        }
        DebugProbeType::STLink => {
            let mut link = stlink::STLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            if opt.nrf_recover {
                return Err(format_err!("It isn't possible to recover with a ST-Link"));
            }
            MasterProbe::from_specific_probe(link)
        }
    };

    let strategy = if let Some(identifier) = opt.chip {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let mut registry = Registry::from_builtin_families();
    if let Some(cdp) = opt.chip_description_path {
        registry.add_target_from_yaml(&Path::new(&cdp))?;
    }

    let target = registry.get_target(strategy)?;

    let mut session = Session::new(target, probe);

    // Start timer.
    let instant = Instant::now();

    let mm = session.target.memory_map.clone();
    download_file(
        &mut session,
        std::path::Path::new(&path_str.to_string().as_str()),
        Format::Elf,
        &mm,
    )
    .map_err(|e| format_err!("failed to flash {}: {}", path_str, e))?;

    // Stop timer.
    let elapsed = instant.elapsed();
    println!(
        "    {} in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0
    );

    session.target.core.reset(&mut session.probe)?;

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
        for algorithms in family.algorithms() {
            println!("        {} ({})", algorithms.name, algorithms.description);
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
    ProbeRs(Error),
    StdIO(std::io::Error),
    Quit,
}

impl error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        use crate::DownloadError::*;

        match self {
            ProbeRs(ref e) => Some(e),
            StdIO(ref e) => Some(e),
            Quit => None,
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use crate::DownloadError::*;

        match self {
            ProbeRs(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            Quit => write!(f, "Quit error..."),
        }
    }
}

impl From<Error> for DownloadError {
    fn from(error: Error) -> Self {
        DownloadError::ProbeRs(error)
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        DownloadError::StdIO(error)
    }
}
