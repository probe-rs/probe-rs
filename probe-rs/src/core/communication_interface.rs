use crate::config::ChipInfo;
use crate::error::Error;

pub trait CommunicationInterface {
    fn probe_for_chip_info(self) -> Result<Option<ChipInfo>, Error>;
}
