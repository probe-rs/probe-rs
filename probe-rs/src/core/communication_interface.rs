use crate::config::ChipInfo;
use crate::error::Error;
use crate::Core;

pub trait CommunicationInterface {
    fn probe_for_chip_info(self, core: &mut Core) -> Result<Option<ChipInfo>, Error>;
}
