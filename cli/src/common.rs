use std::path::Path;
use std::fs::File;

use ocd::session::Session;
use ocd::probe::target::Target;
use ocd::coresight::{
    access_ports::{
        AccessPortError,
    },
};

use ron;

use ocd::probe::debug_probe::{
    MasterProbe,
    DebugProbe,
    FakeProbe,
    DebugProbeError,
    DebugProbeType,
};

use std::error::Error; 
use std::fmt;

#[derive(Debug)]
pub enum CliError {
    DebugProbe(DebugProbeError),
    AccessPort(AccessPortError),
    StdIO(std::io::Error),
    Quit,
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use CliError::*;

        match self {
            DebugProbe(ref e) => Some(e),
            AccessPort(ref e) => Some(e),
            StdIO(ref e) => Some(e),
            Quit => None,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CliError::*;

        match self {
            DebugProbe(ref e) => e.fmt(f),
            AccessPort(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            Quit => write!(f, "Quit error..."),
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


/// Takes a closure that is handed an `DAPLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
pub fn with_device<F>(n: usize, target: Target, f: F) -> Result<(), CliError>
where
    for<'a> F: FnOnce(Session) -> Result<(), CliError>
{
    let device = {
        let mut list = ocd::probe::daplink::tools::list_daplink_devices();
        list.extend(ocd::probe::stlink::tools::list_stlink_devices());

        list.remove(n)
    };

    let probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = ocd::probe::daplink::DAPLink::new_from_probe_info(&device)?;

            link.attach(Some(ocd::probe::protocol::WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
        DebugProbeType::STLink => {
            let mut link = ocd::probe::stlink::STLink::new_from_probe_info(&device)?;

            link.attach(Some(ocd::probe::protocol::WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
    };
    
    let session = Session::new(target, probe);

    f(session)
}

pub fn with_dump<F>(p: &Path, f: F) -> Result<(), CliError>
where
    for<'a> F: FnOnce(Session) -> Result<(), CliError>
{
    let mut dump_file = File::open(p)?;

    let dump = ron::de::from_reader(&mut dump_file).unwrap();


    let core = ocd::probe::target::m0::FakeM0::new(dump);
    let fake_probe = FakeProbe::new();

    let probe = MasterProbe::from_specific_probe(Box::new(fake_probe));

    let mut target = ocd::probe::target::nrf51822::nRF51822();
    target.core = Box::new(core);

    let session = Session::new(target, probe);

    f(session)
}
