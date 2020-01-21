use crate::SharedOptions;

use probe_rs::{
    architecture::arm::ap::AccessPortError,
    architecture::arm::m0::FakeM0,
    config::registry::{Registry, RegistryError, SelectionStrategy},
    flash::download::FileDownloadError,
    Core, DebugProbeError, Error, Probe, Session,
};

use ron;
use std::fmt;
use std::fs::File;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    DebugProbe(
        #[source]
        #[from]
        DebugProbeError,
    ),
    AccessPort(
        #[source]
        #[from]
        AccessPortError,
    ),
    StdIO(
        #[source]
        #[from]
        std::io::Error,
    ),
    FileDownload(
        #[source]
        #[from]
        FileDownloadError,
    ),
    RegistryError(
        #[source]
        #[from]
        RegistryError,
    ),
    MissingArgument,
    UnableToOpenProbe(Option<&'static str>),
    ProbeRs(
        #[source]
        #[from]
        Error,
    ),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CliError::*;

        match self {
            DebugProbe(ref e) => e.fmt(f),
            AccessPort(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            FileDownload(ref e) => e.fmt(f),
            RegistryError(ref e) => e.fmt(f),
            MissingArgument => write!(f, "Command expected more arguments."),
            UnableToOpenProbe(ref details) => match details {
                None => write!(f, "Unable to open probe."),
                Some(details) => write!(f, "Unable to open probe: {}", details),
            },
            ProbeRs(ref e) => e.fmt(f),
        }
    }
}

pub(crate) fn open_probe(index: Option<usize>) -> Result<Probe, CliError> {
    let available_probes = Probe::list_all();

    let device = match index {
        Some(index) => available_probes
            .get(index)
            .ok_or(CliError::UnableToOpenProbe(Some("Unable to open the specified probe. Use the 'list' subcommand to see all available probes.")))?,
        None => {
            // open the default probe, if only one probe was found
            if available_probes.len() == 1 {
                &available_probes[0]
            } else {
                return Err(CliError::UnableToOpenProbe(Some("Multiple probes found. Please specify which probe to use using the -n parameter.")));
            }
        }
    };

    let probe = Probe::from_probe_info(&device)?;

    Ok(probe)
}

/// Takes a closure that is handed an `DAPLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
pub(crate) fn with_device<F>(shared_options: &SharedOptions, f: F) -> Result<(), CliError>
where
    for<'a> F: FnOnce(Session) -> Result<(), CliError>,
{
    let mut probe = open_probe(shared_options.n)?;

    let strategy = if let Some(identifier) = &shared_options.target {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        eprintln!("Autodetection of the target is currently disabled for stability reasons.");
        std::process::exit(1);
        // TODO:
        // SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let registry = Registry::from_builtin_families();

    let target = registry.get_target(strategy)?;

    let session = probe.attach(target, None)?;

    f(session)
}

pub(crate) fn with_dump<F>(shared_options: &SharedOptions, p: &Path, f: F) -> Result<(), CliError>
where
    for<'a> F: FnOnce(Session, Option<Core>) -> Result<(), CliError>,
{
    let mut dump_file = File::open(p)?;

    let dump = ron::de::from_reader(&mut dump_file).unwrap();

    let mut probe = Probe::new_dummy();

    let strategy = if let Some(identifier) = &shared_options.target {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        unimplemented!();
    };

    let registry = Registry::from_builtin_families();

    let target = registry.get_target(strategy)?;

    // TODO: fix this
    // target.core = Box::new(core);

    let session = probe.attach(target, None)?;
    let core = session.attach_to_specific_core(FakeM0::new(dump))?;

    f(session, Some(core))
}
