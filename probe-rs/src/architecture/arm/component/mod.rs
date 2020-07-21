mod dwt;
mod itm;
mod tpiu;

use super::memory::romtable::Component;
use crate::architecture::arm::core::m0::Demcr;
use crate::architecture::arm::{SwoConfig, SwoMode};
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

pub fn setup_swv(core: &mut Core, component: &Component, config: &SwoConfig) -> Result<(), Error> {
    // Enable tracing
    enable_tracing(core)?;

    // Perform vendor-specific SWV setup
    setup_swv_vendor(core, component, config)?;

    // Configure TPIU
    let mut tpiu = component.tpiu(core).map_err(Error::architecture_specific)?;
    tpiu.set_port_size(1)?;
    let prescaler = (config.tpiu_clk / config.baud) - 1;
    tpiu.set_prescaler(prescaler)?;
    match config.mode {
        SwoMode::Manchester => tpiu.set_pin_protocol(1)?,
        SwoMode::UART => tpiu.set_pin_protocol(2)?,
    }
    // Formatter: TrigIn enabled, continuous formatting disabled (aka bypass mode)
    tpiu.set_formatter(0x100)?;

    // Configure ITM
    let mut itm = component.itm(core).map_err(Error::architecture_specific)?;
    itm.unlock()?;
    itm.tx_enable()?;

    // Configure DWT
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    dwt.enable()?;

    Ok(())
}

fn setup_swv_vendor(core: &mut Core, component: &Component, _config: &SwoConfig) -> Result<(), Error> {
    match component.id().peripheral_id().jep106() {
        Some(id) if id == jep106::JEP106Code::new(0x00, 0x20) => {
            // STMicroelectronics:
            // STM32 parts need TRACE_IOEN set to 1 and TRACE_MODE set to 00.
            log::debug!("STMicroelectronics part detected, configuring DBGMCU");
            const DBGMCU: u32 = 0xE004_2004;
            let mut dbgmcu = core.read_word_32(DBGMCU)?;
            dbgmcu |= 1 << 5;
            dbgmcu &= !(0b00 << 6);
            core.write_word_32(DBGMCU, dbgmcu)
        },
        _ => Ok(()),
    }
}

/// Configures DWT trace unit `unit` to begin tracing `address`.
pub fn add_swv_data_trace(
    core: &mut Core,
    component: &Component,
    unit: usize,
    address: u32,
) -> Result<(), Error> {
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    dwt.enable_data_trace(unit, address)
}

pub fn remove_swv_data_trace(core: &mut Core, component: &Component, unit: usize) -> Result<(), Error> {
    let mut dwt = component.dwt(core).map_err(Error::architecture_specific)?;
    dwt.disable_data_trace(unit)
}

/// Sets TRCENA in DEMCR to begin trace generation.
pub fn enable_tracing(core: &mut Core) -> Result<(), Error> {
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
    demcr.set_dwtena(true);
    core.write_word_32(Demcr::ADDRESS, demcr.into())?;
    Ok(())
}

/// Disables TRCENA in DEMCR to disable trace generation.
pub fn disable_swv(core: &mut Core) -> Result<(), Error> {
    let mut demcr = Demcr(core.read_word_32(Demcr::ADDRESS)?);
    demcr.set_dwtena(false);
    core.write_word_32(Demcr::ADDRESS, demcr.into())?;
    Ok(())
}
