extern crate structopt;

use std::{
    time::Instant,
    path::{
        PathBuf,
    },
    process::{
        Command,
        Stdio,
    },
    error::Error,
    fmt,
};

use structopt::StructOpt;
use colored::*;

use ocd::{
    coresight::{
        access_ports::{
            AccessPortError,
        },
    },
    probe::{
        debug_probe::{
            MasterProbe,
            DebugProbe,
            DebugProbeError,
            DebugProbeType,
        },
        flash::download::{
            FileDownloader,
            Format,
        },
        daplink,
        stlink,
        protocol::WireProtocol
    },
    session::Session,
    target::{
        Target,
        select_target_graceful_exit,
    },
};

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(name = "binary", long="bin")]
    bin: Option<String>,
    #[structopt(name = "example", long="example")]
    example: Option<String>,
    #[structopt(name = "package", short="p", long="package")]
    package: Option<String>,
    #[structopt(name = "release", long="release")]
    release: bool,
    #[structopt(name = "target", long="target")]
    target: Option<String>,
    #[structopt(name = "chip", long="chip")]
    chip: Option<String>,
    #[structopt(name = "PATH", long="manifest-path", parse(from_os_str))]
    manifest_path: Option<PathBuf>,
}

fn main() {
    match main_try() {
        Ok(_) => (),
        Err(e) => println!("{}", e),
    }
}

fn main_try() -> Result<(), failure::Error> {
    let mut args = std::env::args();
    // Skip the first arg which is the calling application name.
    let _ = args.next();

    // Get commandline options.
    let opt = Opt::from_iter(args);

    // Try and get the cargo project information.
    let project = cargo_project::Project::query(".")?;

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
        "x86_64-unknown-linux-gnu"
    )?;

    let path_str = match path.to_str() {
        Some(s) => s,
        None => panic!(),
    };

    let mut args: Vec<_> = std::env::args().collect();
    // Remove first two args which is the calling application name and the `flash` command from cargo.
    args.remove(0);
    args.remove(0);
    // Remove possible `--chip <chip>` arguments as cargo build does not understand it.
    if let Some(index) = args.iter().position(|x| *x == "--chip") {
        args.remove(index);
        args.remove(index);
    }

    Command::new("cargo")
        .arg("build")
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?
        .wait()?;
    
    println!("    {} {}", "Flashing".green().bold(), path_str);

    download_program_fast(0, select_target_graceful_exit(opt.chip), path_str.to_string())?;

    Ok(())
}

fn download_program_fast(n: usize, target: Target, path: String) -> Result<(), DownloadError> {
    with_device(n as usize, target, |mut session| {

        // Start timer.
        let instant = Instant::now();

        let mm = session.target.memory_map.clone();
        let fd = FileDownloader::new();
        fd.download_file(
            &mut session,
            std::path::Path::new(&path.as_str()),
            Format::Elf,
            &mm
        ).unwrap();

        let r = Ok(());

        // Stop timer.
        let elapsed = instant.elapsed();
        println!("    {} in {}s", "Finished".green().bold(), elapsed.as_millis() as f32 / 1000.0);

        r
    })
}

/// Takes a closure that is handed an `DAPLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
pub fn with_device<F>(n: usize, target: Target, f: F) -> Result<(), DownloadError>
where
    for<'a> F: FnOnce(Session) -> Result<(), DownloadError>
{
    let device = {
        let mut list = daplink::tools::list_daplink_devices();
        list.extend(stlink::tools::list_stlink_devices());

        list.remove(n)
    };

    let probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = daplink::DAPLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
        DebugProbeType::STLink => {
            let mut link = stlink::STLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
    };
    
    let session = Session::new(target, probe);

    f(session)
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