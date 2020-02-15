mod dwt;
mod itm;
mod tpiu;

use super::memory::romtable::Component;
use crate::{Core, Error};
pub use dwt::Dwt;
pub use itm::Itm;
pub use tpiu::Tpiu;

pub fn setup_tracing(core: &mut Core, component: &Component) -> Result<(), Error> {
    // stm32 specific reg (DBGMCU_CR):
    core.write_word_32(0xE004_2004, 0x27)?;

    // Config tpiu:
    let mut tpiu = component.tpiu(core).map_err(Error::architecture_specific)?;
    tpiu.set_port_size(1)?;
    let uc_freq = 16; // MHz (HSI frequency)
    let swo_freq = 2; // MHz
    let prescaler = (uc_freq / swo_freq) - 1;
    tpiu.set_prescaler(prescaler)?;
    tpiu.set_pin_protocol(2)?;
    tpiu.set_formatter(0x100)?;

    // Config itm:
    let mut itm = component.itm(core).map_err(Error::architecture_specific)?;
    itm.unlock()?;
    itm.tx_enable()?;

    // config dwt:
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    dwt.setup_tracing()?;

    Ok(())
}

pub fn start_trace_memory_address(
    core: &mut Core,
    component: &Component,
    addr: u32,
) -> Result<(), Error> {
    // config dwt:
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    // Future:
    dwt.enable_trace(addr)?;
    // dwt.disable_memory_watch()?;

    Ok(())
}
