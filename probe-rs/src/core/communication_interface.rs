use crate::config::chip_info::ChipInfo;
use crate::error::Error;
use crate::probe::Probe;

pub trait CommunicationInterface {
    fn probe_for_chip_info(self) -> Result<Option<ChipInfo>, Error>;
}
