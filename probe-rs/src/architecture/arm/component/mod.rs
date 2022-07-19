//! Types and functions for interacting with CoreSight Components

mod dwt;
mod itm;
mod swo;
mod tmc;
mod tpiu;
mod trace_funnel;

use super::memory::romtable::{CoresightComponent, PeripheralType, RomTableError};
use crate::architecture::arm::core::armv6m::Demcr;
use crate::architecture::arm::{ArmProbeInterface, SwoConfig, SwoMode};
use crate::{Core, Error, MemoryInterface, MemoryMappedRegister};
use anyhow::anyhow;
use std::io::{Read, Seek, Write};

pub use dwt::Dwt;
pub use itm::Itm;
pub use swo::Swo;
pub use tmc::TraceMemoryController;
pub use tpiu::Tpiu;
pub use trace_funnel::TraceFunnel;

/// Specifies the data sink (destination) for trace data.
pub enum TraceSink {
    /// Trace data should be sent to the SWO peripheral.
    ///
    /// # Note
    /// On some architectures, there is no distinction between SWO and TPIU.
    Swo(SwoConfig),

    /// Trace data should be sent to the TPIU peripheral.
    Tpiu(SwoConfig),

    /// Trace data should be sent to the embedded trace buffer for software-based trace collection.
    TraceMemory,
}

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
pub(crate) fn setup_tracing(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
    sink: &TraceSink,
) -> Result<(), Error> {
    // Configure DWT
    let mut dwt = Dwt::new(interface, find_component(components, PeripheralType::Dwt)?);
    dwt.enable()?;
    dwt.enable_exception_trace()?;

    // Configure ITM
    let mut itm = Itm::new(interface, find_component(components, PeripheralType::Itm)?);
    itm.unlock()?;
    itm.tx_enable()?;

    // Configure the trace destination.
    match sink {
        TraceSink::Tpiu(config) => {
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
        }

        TraceSink::Swo(config) => {
            let mut swo = Swo::new(interface, find_component(components, PeripheralType::Swo)?);
            swo.unlock()?;

            let prescaler = (config.tpiu_clk() / config.baud()) - 1;
            swo.set_prescaler(prescaler)?;

            match config.mode() {
                SwoMode::Manchester => swo.set_pin_protocol(1)?,
                SwoMode::Uart => swo.set_pin_protocol(2)?,
            }
        }

        TraceSink::TraceMemory => {
            let mut tmc = TraceMemoryController::new(
                interface,
                find_component(components, PeripheralType::Tmc)?,
            );

            // Clear out the TMC FIFO before initiating the capture.
            tmc.disable_capture()?;
            while !tmc.ready()? {}

            // Configure the TMC for software-polled mode, as we will read out data using the debug
            // interface.
            tmc.set_mode(tmc::Mode::Software)?;

            tmc.enable_capture()?;
        }
    }

    Ok(())
}

pub(crate) fn read_trace_memory(
    interface: &mut Box<dyn ArmProbeInterface>,
    components: &[CoresightComponent],
) -> Result<Vec<u8>, Error> {
    let mut tmc =
        TraceMemoryController::new(interface, find_component(components, PeripheralType::Tmc)?);

    // TODO: In the future, it may be possible to dynamically read from trace memory
    // without waiting for the FIFO to fill first.
    let fifo_size = tmc.fifo_size()?;
    while !tmc.full()? {
        log::info!("TMC FIFO Level: {} of {fifo_size}", tmc.fill_level()?)
    }

    // This sequence is taken from "CoreSight Trace memory Controller Technical Reference Manual"
    // Section 2.2.2 "Software FIFO Mode". Without following this procedure, the trace data does
    // not properly stop even after disabling capture.
    tmc.stop_on_flush(true)?;
    tmc.manual_flush()?;

    // Read all of the data from the ETM into a vector for further processing.
    let mut etf_trace = std::io::Cursor::new(vec![0; tmc.fifo_size()? as usize + 128]);
    loop {
        if let Some(data) = tmc.read()? {
            etf_trace
                .write_all(&data.to_le_bytes())
                .map_err(|e| anyhow!("Failed to write TMC data buffer: {e}"))?;
        } else if tmc.ready()? {
            break;
        }
    }

    assert!(tmc.empty()?);
    tmc.disable_capture()?;

    // The TMC formats data into frames, as it contains trace data from multiple data sources. We
    // need to deserialize the frames and pull out only the data source of interest. For now, all
    // we care about is the ITM data.
    etf_trace
        .rewind()
        .map_err(|e| anyhow!("Failed to rewind ETF trace data: {e}"))?;

    let mut id = 0.into();
    let mut frame_buffer = [0u8; 16];

    let mut itm_trace = Vec::new();

    loop {
        match etf_trace.read_exact(&mut frame_buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            other => other,
        }
        .map_err(|e| anyhow!("Failed to read ETF trace data: {e}"))?;

        let mut frame = tmc::Frame::new(&frame_buffer, id);
        for (id, data) in &mut frame {
            match id.into() {
                // ITM ATID, see Itm::tx_enable()
                13 => itm_trace.push(data),
                0 => (),
                id => log::warn!("Unexpected trace source ATID {id}: {data}, ignoring"),
            }
        }
        id = frame.id();
    }

    Ok(itm_trace)
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
