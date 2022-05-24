//! Types and functions for interacting with CoreSight Components

mod dwt;
mod itm;
mod swo;
mod tpiu;
mod trace_funnel;

use super::memory::romtable::{CoresightComponent, PeripheralType, RomTableError};
use crate::architecture::arm::core::armv6m::Demcr;
use crate::architecture::arm::{ap::AccessPort, ArmProbeInterface, SwoConfig, SwoMode};
use crate::{Core, CoreRegister, Error, MemoryInterface};
pub use dwt::Dwt;
pub use itm::Itm;
pub use swo::Swo;
pub use tpiu::Tpiu;
pub use trace_funnel::TraceFunnel;

/// An error when operating a core ROM table component occurred.
#[derive(thiserror::Error, Debug)]
pub enum ComponentError {
    /// Nordic chips do not support setting all TPIU clocks. Try choosing another clock speed.
    #[error("Nordic does not support TPIU CLK value of {0}")]
    NordicUnsupportedTPUICLKValue(u32),
}

/// A trait to be implemented on debug register types for debug component interfaces.
pub trait DebugRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    /// The address of the register.
    const ADDRESS: u32;
    /// The name of the register.
    const NAME: &'static str;

    /// Loads the register value from the given debug component via the given core.
    fn load(
        component: &CoresightComponent,
        interface: &mut Box<dyn ArmProbeInterface>,
    ) -> Result<Self, Error> {
        Ok(Self::from(component.read_reg(interface, Self::ADDRESS)?))
    }

    /// Loads the register value from the given component in given unit via the given core.
    fn load_unit(
        component: &CoresightComponent,
        interface: &mut Box<dyn ArmProbeInterface>,
        unit: usize,
    ) -> Result<Self, Error> {
        Ok(Self::from(
            component.read_reg(interface, Self::ADDRESS + 16 * unit as u32)?,
        ))
    }

    /// Stores the register value to the given debug component via the given core.
    fn store(
        &self,
        component: &CoresightComponent,
        interface: &mut Box<dyn ArmProbeInterface>,
    ) -> Result<(), Error> {
        component.write_reg(interface, Self::ADDRESS, self.clone().into())
    }

    /// Stores the register value to the given component in given unit via the given core.
    fn store_unit(
        &self,
        component: &CoresightComponent,
        interface: &mut Box<dyn ArmProbeInterface>,
        unit: usize,
    ) -> Result<(), Error> {
        component.write_reg(
            interface,
            Self::ADDRESS + 16 * unit as u32,
            self.clone().into(),
        )
    }
}

/// Goes through every component in the vector and tries to find the first component with the given type
fn find_component(
    components: &[CoresightComponent],
    peripheral_type: PeripheralType,
) -> Result<&CoresightComponent, Error> {
    components
        .iter()
        .find_map(|component| component.find_component(peripheral_type))
        .ok_or_else(|| {
            Error::architecture_specific(RomTableError::ComponentNotFound(peripheral_type))
        })
}

/// Sets up all the SWV components.
///
/// Expects to be given a list of all ROM table `components` as the second argument.
pub(crate) fn setup_swv(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
    config: &SwoConfig,
) -> Result<(), Error> {
    log::info!("Configuring TPIU");
    // Configure TPIU
    let mut tpiu = Tpiu::new(interface, find_component(components, PeripheralType::Tpiu)?);

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

    // Configure SWO - it may not be present in some architectures, as the TPIU may drive SWO.
    if let Ok(component) = find_component(components, PeripheralType::Swo) {
        log::info!("Configuring SWO");
        let mut swo = Swo::new(interface, component);
        swo.unlock()?;

        swo.set_prescaler(prescaler)?;

        match config.mode() {
            SwoMode::Manchester => swo.set_pin_protocol(1)?,
            SwoMode::Uart => swo.set_pin_protocol(2)?,
        }
    } else {
        log::warn!("SWO component not found - assuming TPIU-only configuration");
    }

    // Enable all ports of any trace funnels found.
    log::info!("Enabling all trace funnels");
    for trace_funnel in components
        .iter()
        .filter_map(|comp| comp.find_component(PeripheralType::TraceFunnel))
    {
        let mut funnel = TraceFunnel::new(interface, trace_funnel);
        funnel.unlock()?;
        funnel.enable_port(0xFF)?;
    }

    // Configure ITM
    log::info!("Enabling ITM");
    let mut itm = Itm::new(interface, find_component(components, PeripheralType::Itm)?);
    itm.unlock()?;
    itm.tx_enable()?;

    // Configure DWT
    log::info!("Enabling DWT");
    let mut dwt = Dwt::new(interface, find_component(components, PeripheralType::Dwt)?);
    dwt.enable()?;
    dwt.enable_exception_trace()?;

    // TODO: Replace flush
    //interface.flush()
    Ok(())
}

/// Sets up all vendor specific bit of all the SWV components.
///
/// Expects to be given a list of all ROM table `components` as the second argument.
pub(crate) fn setup_swv_vendor(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
    config: &SwoConfig,
) -> Result<(), Error> {
    if components.is_empty() {
        return Err(Error::architecture_specific(RomTableError::NoComponents));
    }

    for component in components.iter() {
        let mut memory = interface.memory_interface(component.ap)?;

        let peripheral = component.component.id().peripheral_id();

        match peripheral.jep106() {
            Some(id) if id == jep106::JEP106Code::new(0x00, 0x20) => {
                // STMicroelectronics:
                log::info!("STMicroelectronics part detected, configuring DBGMCU");
                match peripheral.part() {
                    // H7 parts
                    0x450 => {
                        // The DBGMCU is only accessible on AP2 for the H7.
                        if component.ap.ap_address().ap != 2 {
                            continue;
                        }
                        log::info!("Configuring STM32H7 DBGMCU");
                        const DBGMCU: u32 = 0xE00E_1000;
                        let mut dbgmcu_cr = memory.read_word_32(DBGMCU + 0x04)?;
                        // Enable domain 3 debug clock.
                        dbgmcu_cr |= 1 << 22;

                        // Enable domain 1 debug clock.
                        dbgmcu_cr |= 1 << 21;

                        // Enable TPIU clock.
                        dbgmcu_cr |= 1 << 20;

                        // Allow debugging in standby mode
                        dbgmcu_cr |= 1 << 2;

                        return memory.write_word_32(DBGMCU + 0x04, dbgmcu_cr);
                    }

                    _ => {
                        // F4/F7 parts need TRACE_IOEN set to 1 and TRACE_MODE set to 00.
                        log::info!("Configuring default DBGMCU");
                        const DBGMCU: u32 = 0xE004_2004;
                        let mut dbgmcu = memory.read_word_32(DBGMCU)?;
                        dbgmcu |= 1 << 5;
                        dbgmcu &= !(0b00 << 6);
                        return memory.write_word_32(DBGMCU, dbgmcu);
                    }
                }
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
                return memory.write_word_32(CLOCK_TRACECONFIG, traceconfig);
            }
            _ => {
                continue;
            }
        }
    }

    Ok(())
}

/// Configures DWT trace unit `unit` to begin tracing `address`.
///
///
/// Expects to be given a list of all ROM table `components` as the second argument.
pub(crate) fn add_swv_data_trace(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
    unit: usize,
    address: u32,
) -> Result<(), Error> {
    let mut dwt = Dwt::new(interface, find_component(components, PeripheralType::Dwt)?);
    dwt.enable_data_trace(unit, address)
}

/// Configures DWT trace unit `unit` to stop tracing `address`.
///
///
/// Expects to be given a list of all ROM table `components` as the second argument.
pub fn remove_swv_data_trace(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
    unit: usize,
) -> Result<(), Error> {
    let mut dwt = Dwt::new(interface, find_component(components, PeripheralType::Dwt)?);
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
