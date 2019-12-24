use crate::SharedOptions;

use probe_rs::{
    config::registry::{Registry, SelectionStrategy},
    cores::m0::FakeM0,
    probe::{daplink, stlink, DebugProbe, DebugProbeType, FakeProbe, MasterProbe, WireProtocol},
    target::info::ChipInfo,
    Error, Session,
};

use ron;

use std::error;
use std::fmt;
use std::fs::File;
use std::path::Path;

#[derive(Debug)]
pub enum CliError {
    StdIO(std::io::Error),
    ProbeRs(Error),
    MissingArgument,
    UnableToOpenProbe,
}

impl error::Error for CliError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        use CliError::*;

        match self {
            StdIO(ref e) => Some(e),
            ProbeRs(ref e) => Some(e),
            MissingArgument => None,
            UnableToOpenProbe => None,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CliError::*;

        match self {
            StdIO(ref e) => e.fmt(f),
            ProbeRs(ref e) => e.fmt(f),
            MissingArgument => write!(f, "Command expected more arguments."),
            UnableToOpenProbe => write!(f, "Unable to open probe."),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::StdIO(error)
    }
}

impl From<probe_rs::Error> for CliError {
    fn from(error: probe_rs::Error) -> Self {
        CliError::ProbeRs(error)
    }
}

pub(crate) fn open_probe(index: Option<usize>) -> Result<MasterProbe, CliError> {
    let mut list = daplink::tools::list_daplink_devices();
    list.extend(stlink::tools::list_stlink_devices());

    let device = match index {
        Some(index) => list.get(index).ok_or(CliError::UnableToOpenProbe)?,
        None => {
            // open the default probe, if only one probe was found
            if list.len() == 1 {
                &list[0]
            } else {
                return Err(CliError::UnableToOpenProbe);
            }
        }
    };

    let probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = daplink::DAPLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            MasterProbe::from_specific_probe(link)
        }
        DebugProbeType::STLink => {
            let mut link = stlink::STLink::new_from_probe_info(&device)?;

            link.attach(Some(WireProtocol::Swd))?;

            MasterProbe::from_specific_probe(link)
        }
    };

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
        SelectionStrategy::ChipInfo(ChipInfo::read_from_rom_table(&mut probe)?)
    };

    let registry = Registry::from_builtin_families();

    let target = registry.get_target(strategy).map_err(Error::from)?;

    let session = Session::new(target, probe);

    f(session)
}

pub(crate) fn with_dump<F>(shared_options: &SharedOptions, p: &Path, f: F) -> Result<(), CliError>
where
    for<'a> F: FnOnce(Session) -> Result<(), CliError>,
{
    let mut dump_file = File::open(p)?;

    let dump = ron::de::from_reader(&mut dump_file).unwrap();

    let core = FakeM0::new(dump);
    let fake_probe = FakeProbe::new();

    let probe = MasterProbe::from_specific_probe(Box::new(fake_probe));

    let strategy = if let Some(identifier) = &shared_options.target {
        SelectionStrategy::TargetIdentifier(identifier.into())
    } else {
        unimplemented!();
    };

    let registry = Registry::from_builtin_families();

    let mut target = registry.get_target(strategy).map_err(Error::from)?;

    target.core = Box::new(core);

    let session = Session::new(target, probe);

    f(session)
}
