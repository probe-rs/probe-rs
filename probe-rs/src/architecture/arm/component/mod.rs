mod dwt;
mod itm;
mod tpiu;

use super::memory::romtable::{Component, PeripheralType, RomTableError};
use crate::architecture::arm::core::m0::Demcr;
use crate::architecture::arm::{SwoConfig, SwoMode};
use crate::core::CoreRegister;
use crate::{Core, Error, MemoryInterface};
pub use dwt::Dwt;
pub use itm::Itm;
pub use tpiu::Tpiu;

#[derive(thiserror::Error, Debug)]
pub enum ComponentError {
    #[error("Nordic does not support TPIU CLK value of {0}")]
    NordicUnsupportedTPUICLKValue(u32),
}

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

/// Goes through every component in the vector and tries to find the first component with the given type
fn find_component(
    components: &[Component],
    peripheral_type: PeripheralType,
) -> Result<&Component, Error> {
    components
        .iter()
        .find_map(|component| component.find_component(peripheral_type))
        .ok_or_else(|| {
            Error::architecture_specific(RomTableError::ComponentNotFound(peripheral_type))
        })
}

pub fn setup_swv(
    core: &mut Core,
    components: &[Component],
    config: &SwoConfig,
) -> Result<(), Error> {
    // Enable tracing
    enable_tracing(core)?;

    // Perform vendor-specific SWV setup
    setup_swv_vendor(core, components, config)?;

    // Configure TPIU
    let mut tpiu = Tpiu::new(core, find_component(components, PeripheralType::Tpiu)?);

    tpiu.set_port_size(1)?;
    let prescaler = (config.tpiu_clk() / config.baud()) - 1;
    tpiu.set_prescaler(prescaler)?;
    match config.mode() {
        SwoMode::Manchester => tpiu.set_pin_protocol(1)?,
        SwoMode::Uart => tpiu.set_pin_protocol(2)?,
    }

    // Formatter: TrigIn enabled, bypass optional
    if config.tpiu_continuous_formatting() {
        // Set EnFCont for continuous formatting even over SWO.
        tpiu.set_formatter(0x102)?;
    } else {
        // Clear EnFCont to only pass through raw ITM/DWT data.
        tpiu.set_formatter(0x100)?;
    }

    // Configure ITM
    let mut itm = Itm::new(core, find_component(components, PeripheralType::Itm)?);
    itm.unlock()?;
    itm.tx_enable()?;

    // Configure DWT
    let mut dwt = Dwt::new(core, find_component(components, PeripheralType::Dwt)?);
    dwt.enable()?;
    dwt.enable_exception_trace()?;

    core.flush()
}

fn setup_swv_vendor(
    core: &mut Core,
    components: &[Component],
    config: &SwoConfig,
) -> Result<(), Error> {
    if components.is_empty() {
        return Err(Error::architecture_specific(RomTableError::NoComponents));
    }

    for component in components.iter() {
        match component.id().peripheral_id().jep106() {
            Some(id) if id == jep106::JEP106Code::new(0x00, 0x20) => {
                // STMicroelectronics:
                // STM32 parts need TRACE_IOEN set to 1 and TRACE_MODE set to 00.
                log::debug!("STMicroelectronics part detected, configuring DBGMCU");
                const DBGMCU: u32 = 0xE004_2004;
                let mut dbgmcu = core.read_word_32(DBGMCU)?;
                dbgmcu |= 1 << 5;
                dbgmcu &= !(0b00 << 6);
                return core.write_word_32(DBGMCU, dbgmcu);
            }
            Some(id) if id == jep106::JEP106Code::new(0x02, 0x44) => {
                // Nordic VLSI ASA
                log::debug!("Nordic part detected, configuring CLOCK TRACECONFIG");
                const CLOCK_TRACECONFIG: u32 = 0x4000_055C;
                let mut traceconfig: u32 = 0;
                traceconfig |= match config.tpiu_clk() {
                    4_000_000 => 3,
                    8_000_000 => 2,
                    16_000_000 => 1,
                    32_000_000 => 0,
                    tpiu_clk => {
                        let e = ComponentError::NordicUnsupportedTPUICLKValue(tpiu_clk);
                        log::error!("{:?}", e);
                        return Err(Error::architecture_specific(e));
                    }
                };
                traceconfig |= 1 << 16; // tracemux : serial = 1
                return core.write_word_32(CLOCK_TRACECONFIG, traceconfig);
            }
            _ => {
                continue;
            }
        }
    }

    Ok(())
}

/// Configures DWT trace unit `unit` to begin tracing `address`.
pub fn add_swv_data_trace(
    core: &mut Core,
    components: &[Component],
    unit: usize,
    address: u32,
) -> Result<(), Error> {
    let mut dwt = Dwt::new(core, find_component(components, PeripheralType::Dwt)?);
    dwt.enable_data_trace(unit, address)
}

pub fn remove_swv_data_trace(
    core: &mut Core,
    components: &[Component],
    unit: usize,
) -> Result<(), Error> {
    let mut dwt = Dwt::new(core, find_component(components, PeripheralType::Dwt)?);
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
