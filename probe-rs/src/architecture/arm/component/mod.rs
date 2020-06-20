mod dwt;
mod itm;
mod tpiu;

use super::memory::romtable::Component;
use crate::architecture::arm::core::m0::Demcr;
use crate::core::CoreRegister;
use crate::{Core, Error, MemoryInterface};
pub use dwt::Dwt;
pub use itm::Itm;
pub use tpiu::Tpiu;

pub trait DebugRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;

    fn load(component: &Component, core: &mut Core) -> Result<Self, Error> {
        Ok(Self::from(component.read_reg(core, Self::ADDRESS)?))
    }

    fn load_unit(component: &Component, core: &mut Core, unit: usize) -> Result<Self, Error> {
        Ok(Self::from(
            component.read_reg(core, Self::ADDRESS + 16 * unit as u32)?,
        ))
    }

    fn store(&self, component: &Component, core: &mut Core) -> Result<(), Error> {
        component.write_reg(core, Self::ADDRESS, self.clone().into())
    }

    fn store_unit(&self, component: &Component, core: &mut Core, unit: usize) -> Result<(), Error> {
        component.write_reg(core, Self::ADDRESS + 16 * unit as u32, self.clone().into())
    }
}

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
    dwt.enable()?;

    Ok(())
}

/// Starts tracing the data at given address.
pub fn enable_data_trace(
    core: &mut Core,
    component: &Component,
    unit: usize,
    address: u32,
) -> Result<(), Error> {
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    dwt.enable_data_trace(unit, address)
}

pub fn trace_enable(core: &mut Core) -> Result<(), Error> {
    // Enable
    // - Data Watchpoint and Trace (DWT)
    // - Instrumentation Trace Macrocell (ITM)
    // - Embedded Trace Macrocell (ETM)
    // - Trace Port Interface Unit (TPIU).
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
    demcr.set_dwtena(true);
    core.write_word_32(Demcr::ADDRESS, demcr.into())?;
    Ok(())
}
