use crate::SharedOptions;

use ocd::{
    collection::cores::m0::FakeM0,
    coresight::access_ports::AccessPortError,
    probe::{
        daplink,
        debug_probe::{DebugProbe, DebugProbeError, DebugProbeType, FakeProbe, MasterProbe},
        flash::{download::FileDownloadError, flasher::AlgorithmSelectionError},
        protocol::WireProtocol,
        stlink,
    },
    session::Session,
    target::info::ChipInfo,
    target::TargetSelectionError,
};
use ocd_targets::SelectionStrategy;

use ron;

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::path::Path;

#[derive(Debug)]
pub enum CliError {
    DebugProbe(DebugProbeError),
    AccessPort(AccessPortError),
    TargetSelectionError(TargetSelectionError),
    StdIO(std::io::Error),
    FlashAlgorithm(AlgorithmSelectionError),
    FileDownload(FileDownloadError),
    MissingArgument,
    UnableToOpenProbe,
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use CliError::*;

        match self {
            DebugProbe(ref e) => Some(e),
            AccessPort(ref e) => Some(e),
            TargetSelectionError(ref e) => Some(e),
            StdIO(ref e) => Some(e),
            MissingArgument => None,
            UnableToOpenProbe => None,
            FlashAlgorithm(ref e) => Some(e),
            FileDownload(ref e) => Some(e),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CliError::*;

        match self {
            DebugProbe(ref e) => e.fmt(f),
            AccessPort(ref e) => e.fmt(f),
            TargetSelectionError(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            FlashAlgorithm(ref e) => e.fmt(f),
            FileDownload(ref e) => e.fmt(f),
            MissingArgument => write!(f, "Command expected more arguments."),
            UnableToOpenProbe => write!(f, "Unable to open probe."),
        }
    }
}

impl From<AccessPortError> for CliError {
    fn from(error: AccessPortError) -> Self {
        CliError::AccessPort(error)
    }
}

impl From<DebugProbeError> for CliError {
    fn from(error: DebugProbeError) -> Self {
        CliError::DebugProbe(error)
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::StdIO(error)
    }
}

impl From<TargetSelectionError> for CliError {
    fn from(error: TargetSelectionError) -> Self {
        CliError::TargetSelectionError(error)
    }
}

impl From<AlgorithmSelectionError> for CliError {
    fn from(error: AlgorithmSelectionError) -> Self {
        CliError::FlashAlgorithm(error)
    }
}

impl From<FileDownloadError> for CliError {
    fn from(error: FileDownloadError) -> Self {
        CliError::FileDownload(error)
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

    let selection_strategy = if let Some(ref target_name) = shared_options.target {
        SelectionStrategy::Name(target_name.clone())
    } else {
        let chip_info = ChipInfo::read_from_rom_table(&mut probe)
            .ok_or(TargetSelectionError::CouldNotAutodetect)?;
        SelectionStrategy::ChipInfo(chip_info)
    };

    let target = ocd_targets::select_target(&selection_strategy)?;

    let flash_algorithm = match target.flash_algorithm {
        Some(ref name) => ocd_targets::select_algorithm(name),
        None => Err(AlgorithmSelectionError::NoAlgorithmSuggested),
    };

    let flash_algorithm = match flash_algorithm {
        Ok(flash_algorithm) => Some(flash_algorithm),
        Err(error) => {
            println!("{:?}", error);
            None
        }
    };

    let session = Session::new(target, probe, flash_algorithm);

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

    let selection_strategy = if let Some(ref target_name) = shared_options.target {
        SelectionStrategy::Name(target_name.clone())
    } else {
        unimplemented!();
    };

    let mut target = ocd_targets::select_target(&selection_strategy)?;

    target.core = Box::new(core);

    let session = Session::new(target, probe, None);

    f(session)
}
