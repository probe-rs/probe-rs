//! Sequences for STM32H7 devices

use std::sync::Arc;

use probe_rs_target::CoreType;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress,
    component::{TraceFunnel, TraceSink},
    memory::{ArmMemoryInterface, CoresightComponent, PeripheralType, romtable::RomTableError},
    sequences::ArmDebugSequence,
};

/// Supported lines for custom sequences on STM32H7xx devices.
#[derive(Debug)]
pub enum Stm32h7Line {
    /// STM32H72x, STM32H73x, STM32H74x, STM32H75x, STM32H7Ax, STM32H7Bx
    H7,

    /// STM32H7Rx, STM32H7Sx
    H7S,
}

// Base address of the trace funnel that directs trace data to the SWO peripheral.
const SWTF_BASE_ADDRESS: u64 = 0xE00E_4000;

// Base address of the trace funnel that directs trace data to the TPIU and ETF
const CSTF_BASE_ADDRESS: u64 = 0xE00F_3000;

/// Specifier for which trace funnel to access.
///
/// # Note
/// The values of the enum are equivalent to the base addresses of the trace funnels.
#[repr(u64)]
#[derive(Copy, Clone, Debug)]
enum TraceFunnelId {
    /// The funnel feeding the SWO peripheral.
    SerialWire = SWTF_BASE_ADDRESS,

    /// The funnel feeding the TPIU and ETF.
    CoreSight = CSTF_BASE_ADDRESS,
}

/// Marker struct indicating initialization sequencing for STM32H7 family parts.
#[derive(Debug)]
pub struct Stm32h7 {
    ap: u8,
}

impl Stm32h7 {
    /// Create the sequencer for the H7 family of parts.
    pub fn create(family: Stm32h7Line) -> Arc<Self> {
        let ap = match family {
            // Most H7 variants have the debug unit on AP2.
            Stm32h7Line::H7 => 2,
            // The H7S/R lack power domain 3 and the third AP; their debug unit is on AP1.
            Stm32h7Line::H7S => 1,
        };
        Arc::new(Self { ap })
    }

    /// Configure all debug components on the chip.
    pub fn enable_debug_components(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        enable: bool,
    ) -> Result<(), ArmError> {
        if enable {
            tracing::info!("Enabling STM32H7 debug components");
        } else {
            tracing::info!("Disabling STM32H7 debug components");
        }

        let mut control = dbgmcu::Control::read(memory)?;

        // There are debug components in the D1 and D2 clock domains. This ensures we can access
        // CoreSight components in these power domains at all times.
        control.enable_d1_clock(enable);
        control.enable_d3_clock(enable);

        // The TRACECK has to be enabled to communicate with the TPIU.
        control.enable_traceck(enable);

        // Configure debug connection in all power modes.
        control.enable_d1_standby_debug(enable);
        control.enable_d1_sleep_debug(enable);
        control.enable_d1_stop_debug(enable);
        control.enable_d2_standby_debug(enable);
        control.enable_d2_sleep_debug(enable);
        control.enable_d2_stop_debug(enable);

        control.write(memory)?;

        Ok(())
    }
}

mod dbgmcu {
    use crate::architecture::arm::ArmError;
    use crate::architecture::arm::memory::ArmMemoryInterface;
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0xE00E_1000;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0433: STM32H7
        /// family reference manual" section 60.5.8
        pub struct Control(u32);
        impl Debug;

        pub u8, dbgsleep_d1, enable_d1_sleep_debug: 0;
        pub u8, dbgstop_d1, enable_d1_stop_debug: 1;
        pub u8, dbgstby_d1, enable_d1_standby_debug: 2;
        pub u8, dbgsleep_d2, enable_d2_sleep_debug: 3;
        pub u8, dbgstop_d2, enable_d2_stop_debug: 4;
        pub u8, dbgstby_d2, enable_d2_standby_debug: 5;

        pub u8, d3dbgcken, enable_d3_clock: 22;
        pub u8, d1dbgcken, enable_d1_clock: 21;
        pub u8, traceclken, enable_traceck: 20;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

        /// Read the control register from memory.
        pub fn read(memory: &mut (impl ArmMemoryInterface + ?Sized)) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(
            &mut self,
            memory: &mut (impl ArmMemoryInterface + ?Sized),
        ) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

/// Get the Coresight component associated with one of the trace funnels.
///
/// # Args
/// * `components` - All of the coresight components discovered on the device.
/// * `trace_funnel` - The ID of the desired trace funnel.
///
/// # Returns
/// The coresight component representing the desired trace funnel.
fn find_trace_funnel(
    components: &[CoresightComponent],
    trace_funnel: TraceFunnelId,
) -> Result<&CoresightComponent, ArmError> {
    components
        .iter()
        .find_map(|comp| {
            comp.iter().find(|component| {
                let id = component.component.id();
                id.peripheral_id().is_of_type(PeripheralType::TraceFunnel)
                    && id.component_address() == trace_funnel as u64
            })
        })
        .ok_or_else(|| {
            ArmError::from(RomTableError::ComponentNotFound(
                PeripheralType::TraceFunnel,
            ))
        })
}

impl ArmDebugSequence for Stm32h7 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        _default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Power up the debug components through selected AP.
        let ap = &FullyQualifiedApAddress::v1_with_default_dp(self.ap);

        let mut memory = interface.memory_interface(ap)?;
        self.enable_debug_components(&mut *memory, true)?;

        Ok(())
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        // Power down the debug components through selected AP.

        self.enable_debug_components(&mut *memory, false)?;

        Ok(())
    }

    fn trace_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        tracing::warn!("Enabling tracing for STM32H7");

        // Configure the two trace funnels in the H7 debug system to route trace data to the
        // appropriate destination. The CSTF feeds the TPIU and ETF peripherals.
        let mut cstf = TraceFunnel::new(
            interface,
            find_trace_funnel(components, TraceFunnelId::CoreSight)?,
        );
        cstf.unlock()?;
        match sink {
            TraceSink::Swo(_) => cstf.enable_port(0b00)?,
            TraceSink::Tpiu(_) | TraceSink::TraceMemory => cstf.enable_port(0b10)?,
        }

        // The SWTF needs to be configured to route traffic to SWO. When not in use, it needs to be
        // disabled so that the SWO peripheral does not propagate buffer overflows through the
        // trace bus via busy signalling.
        let mut swtf = TraceFunnel::new(
            interface,
            find_trace_funnel(components, TraceFunnelId::SerialWire)?,
        );
        swtf.unlock()?;
        if matches!(sink, TraceSink::Swo(_)) {
            swtf.enable_port(0b01)?;
        } else {
            swtf.enable_port(0b00)?;
        }

        Ok(())
    }
}
