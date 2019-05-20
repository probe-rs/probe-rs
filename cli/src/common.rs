use coresight::{
    access_ports::{
        AccessPortError,
        generic_ap::GenericAP,
    },
    ap_access::{
        APAccess,
    },
};
use probe::debug_probe::{
    MasterProbe,
    DebugProbe,
    DebugProbeError,
    DebugProbeType,
};


#[derive(Debug)]
pub enum Error {
    DebugProbe(DebugProbeError),
    AccessPort(AccessPortError),
    Custom(&'static str),
    StdIO(std::io::Error),
}

impl From<AccessPortError> for Error {
    fn from(error: AccessPortError) -> Self {
        Error::AccessPort(error)
    }
}

impl From<DebugProbeError> for Error {
    fn from(error: DebugProbeError) -> Self {
        Error::DebugProbe(error)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::StdIO(error)
    }
}


/// Takes a closure that is handed an `DAPLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
pub fn with_device<F>(n: usize, mut f: F) -> Result<(), Error>
where
    F: FnOnce(&mut MasterProbe) -> Result<(), Error>
{
    let device = {
        let mut list = daplink::tools::list_daplink_devices();
        list.extend(stlink::tools::list_stlink_devices());

        list.remove(n)
    };

    let mut probe = match device.probe_type {
        DebugProbeType::DAPLink => {
            let mut link = daplink::DAPLink::new_from_probe_info(device)?;

            link.attach(Some(probe::protocol::WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
        DebugProbeType::STLink => {
            let mut link = stlink::STLink::new_from_probe_info(device)?;

            link.attach(Some(probe::protocol::WireProtocol::Swd))?;
            
            MasterProbe::from_specific_probe(link)
        },
    };
    
    f(&mut probe)
}