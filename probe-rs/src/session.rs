use crate::architecture::{
    arm::{
        communication_interface::{
            ApInformation::{MemoryAp, Other},
            ArmProbeInterface,
        },
        core::{debug_core_start, reset_catch_clear, reset_catch_set},
        memory::Component,
        SwoConfig,
    },
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::config::{
    ChipInfo, MemoryRegion, RawFlashAlgorithm, RegistryError, Target, TargetSelector,
};
use crate::core::{Architecture, CoreState, SpecificCoreState};
use crate::{AttachMethod, Core, CoreType, DebugProbe, Error, Probe};
use anyhow::anyhow;
use std::time::Duration;

#[derive(Debug)]
pub struct Session {
    target: Target,
    interface: ArchitectureInterface,
    cores: Vec<(SpecificCoreState, CoreState)>,
}

#[derive(Debug)]
enum ArchitectureInterface {
    Arm(Box<dyn ArmProbeInterface>),
    Riscv(RiscvCommunicationInterface),
}

impl From<ArchitectureInterface> for Architecture {
    fn from(value: ArchitectureInterface) -> Self {
        match value {
            ArchitectureInterface::Arm(_) => Architecture::Arm,
            ArchitectureInterface::Riscv(_) => Architecture::Riscv,
        }
    }
}

impl ArchitectureInterface {
    fn attach<'probe>(
        &'probe mut self,
        core: &'probe mut SpecificCoreState,
        core_state: &'probe mut CoreState,
    ) -> Result<Core<'probe>, Error> {
        match self {
            ArchitectureInterface::Arm(state) => {
                let memory = state.memory_interface(0.into())?;

                core.attach_arm(core_state, memory)
            }
            ArchitectureInterface::Riscv(state) => core.attach_riscv(core_state, state),
        }
    }
}

impl<'a> AsMut<dyn DebugProbe + 'a> for ArchitectureInterface {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'a) {
        match self {
            ArchitectureInterface::Arm(interface) => interface.as_mut().as_mut(),
            ArchitectureInterface::Riscv(interface) => interface.as_mut(),
        }
    }
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(
        probe: Probe,
        target: impl Into<TargetSelector>,
        attach_method: AttachMethod,
    ) -> Result<Self, Error> {
        let (probe, target) = get_target_from_selector(target, probe)?;

        let mut session = match target.architecture() {
            Architecture::Arm => {
                let core = (
                    SpecificCoreState::from_core_type(target.core_type),
                    Core::create_state(0),
                );

                let interface = probe.into_arm_interface()?;

                let mut session = Session {
                    target,
                    interface: ArchitectureInterface::Arm(interface.unwrap()),
                    cores: vec![core],
                };

                // Enable debug mode
                debug_core_start(&mut session.core(0)?)?;

                if attach_method == AttachMethod::UnderReset {
                    // we need to halt the chip here
                    reset_catch_set(&mut session.core(0)?)?;

                    // Deassert the reset pin
                    session.interface.as_mut().target_reset_deassert()?;

                    // Wait for the core to be halted
                    let mut core = session.core(0)?;

                    core.wait_for_core_halted(Duration::from_millis(100))?;

                    reset_catch_clear(&mut core)?;
                }

                session
            }
            Architecture::Riscv => {
                // TODO: Handle attach under reset

                let core = (
                    SpecificCoreState::from_core_type(target.core_type),
                    Core::create_state(0),
                );

                let interface = probe.into_riscv_interface()?;

                let mut session = Session {
                    target,
                    interface: ArchitectureInterface::Riscv(interface.unwrap()),
                    cores: vec![core],
                };

                {
                    let mut core = session.core(0)?;

                    core.halt(Duration::from_millis(100))?;
                }

                session
            }
        };

        session.clear_all_hw_breakpoints()?;

        Ok(session)
    }

    /// Automatically creates a session with the first connected probe found.
    pub fn auto_attach(target: impl Into<TargetSelector>) -> Result<Session, Error> {
        // Get a list of all available debug probes.
        let probes = Probe::list_all();

        // Use the first probe found.
        let probe = probes[0].open()?;

        // Attach to a chip.
        probe.attach(target)
    }

    /// Lists the available cores with their number and their type.
    pub fn list_cores(&self) -> Vec<(usize, CoreType)> {
        self.cores
            .iter()
            .map(|(t, _)| CoreType::from(t))
            .enumerate()
            .collect()
    }

    /// Attaches to the core with the given number.
    pub fn core(&mut self, n: usize) -> Result<Core<'_>, Error> {
        let (core, core_state) = self.cores.get_mut(n).ok_or(Error::CoreNotFound(n))?;

        self.interface.attach(core, core_state)
    }

    /// Returns a list of the flash algotithms on the target.
    pub(crate) fn flash_algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.target.flash_algorithms
    }

    pub fn read_swo(&mut self) -> Result<Vec<u8>, Error> {
        let interface = self.get_arm_interface()?;
        interface.read_swo()
    }

    pub fn get_arm_interface(&mut self) -> Result<&mut Box<dyn ArmProbeInterface>, Error> {
        let interface = match &mut self.interface {
            ArchitectureInterface::Arm(state) => state,
            _ => return Err(Error::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        };

        Ok(interface)
    }

    pub fn get_arm_component(&mut self) -> Result<Component, Error> {
        let interface = self.get_arm_interface()?;

        let ap_index = 0;

        let ap_information = interface
            .ap_information(ap_index.into())
            .ok_or_else(|| anyhow!("AP {} does not exist on chip.", ap_index))?;

        match ap_information {
            MemoryAp {
                port_number,
                only_32bit_data_size: _,
                debug_base_address,
            } => {
                let access_port_number = *port_number;
                let base_address = *debug_base_address;

                let mut memory = interface.memory_interface(access_port_number.into())?;

                Component::try_parse(&mut memory, base_address)
                    .map_err(Error::architecture_specific)
            }
            Other { port_number } => {
                // Return an error, only possible to get Component from MemoryAP
                Err(Error::Other(anyhow!(
                    "AP {} is not a MemoryAP, unable to get ARM component.",
                    port_number
                )))
            }
        }
    }

    /// Configure the target and probe for serial wire view (SWV) tracing.
    pub fn setup_swv(&mut self, config: &SwoConfig) -> Result<(), Error> {
        // Configure SWO on the probe
        {
            let interface = self.get_arm_interface()?;
            interface.enable_swo(config)?;
        }

        // Enable tracing on the target
        {
            let mut core = self.core(0)?;
            crate::architecture::arm::component::enable_tracing(&mut core)?;
        }

        // Configure SWV on the target
        let component = self.get_arm_component()?;
        let mut core = self.core(0)?;
        crate::architecture::arm::component::setup_swv(&mut core, &component, config)
    }

    /// Configure the target to stop emitting SWV trace data.
    pub fn disable_swv(&mut self) -> Result<(), Error> {
        crate::architecture::arm::component::disable_swv(&mut self.core(0)?)
    }

    /// Begin tracing a memory address over SWV.
    pub fn add_swv_data_trace(&mut self, unit: usize, address: u32) -> Result<(), Error> {
        let component = self.get_arm_component()?;
        let mut core = self.core(0)?;
        crate::architecture::arm::component::add_swv_data_trace(
            &mut core, &component, unit, address,
        )
    }

    /// Stop tracing from a given SWV unit
    pub fn remove_swv_data_trace(&mut self, unit: usize) -> Result<(), Error> {
        let component = self.get_arm_component()?;
        let mut core = self.core(0)?;
        crate::architecture::arm::component::remove_swv_data_trace(&mut core, &component, unit)
    }

    /// Returns the memory map of the target.
    pub fn memory_map(&self) -> &[MemoryRegion] {
        &self.target.memory_map
    }

    /// Return the `Architecture` of the currently connected chip.
    pub fn architecture(&self) -> Architecture {
        match self.interface {
            ArchitectureInterface::Arm(_) => Architecture::Arm,
            ArchitectureInterface::Riscv(_) => Architecture::Riscv,
        }
    }

    /// Clears all hardware breakpoints on all cores
    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), Error> {
        { 0..self.cores.len() }
            .map(|n| {
                self.core(n)
                    .and_then(|mut core| core.clear_all_hw_breakpoints())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|_| ())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(err) = self.clear_all_hw_breakpoints() {
            log::warn!("Could not clear all hardware breakpoints: {:?}", err);
        }
    }
}
/// Determine the ```Target``` from a ```TargetSelector```.
///
/// If the selector is ```Unspecified```, the target will be looked up in the registry.
/// If it its ```Auto```, probe-rs will try to determine the target automatically, based on
/// information read from the chip.
fn get_target_from_selector(
    target: impl Into<TargetSelector>,
    probe: Probe,
) -> Result<(Probe, Target), Error> {
    let mut probe = Some(probe);

    let target = match target.into() {
        TargetSelector::Unspecified(name) => crate::config::registry::get_target_by_name(name)?,
        TargetSelector::Specified(target) => target,
        TargetSelector::Auto => {
            let mut found_chip = None;

            {
                if probe.as_ref().unwrap().has_arm_interface() {
                    let interface = probe.take().unwrap().into_arm_interface()?;

                    if let Some(mut interface) = interface {
                        //let chip_result = try_arm_autodetect(interface);
                        log::debug!("Autodetect: Trying DAP interface...");

                        let found_arm_chip = interface.read_from_rom_table().unwrap_or_else(|e| {
                            log::info!("Error during auto-detection of ARM chips: {}", e);
                            None
                        });

                        found_chip = found_arm_chip.map(ChipInfo::from);

                        probe = Some(interface.close());
                    } else {
                        //TODO: Handle this case, we still need the probe here!
                        log::debug!("No DAP interface was present. This is not an ARM core. Skipping ARM autodetect.");
                    }
                }
            }

            if found_chip.is_none() && probe.as_ref().unwrap().has_riscv_interface() {
                let interface = probe.take().unwrap().into_riscv_interface()?;

                if let Some(mut interface) = interface {
                    let idcode = interface.read_idcode();

                    log::debug!("ID Code read over JTAG: {:x?}", idcode);

                    probe = Some(interface.close());
                } else {
                    log::debug!("No JTAG interface was present. Skipping Riscv autodetect.");
                }
            }

            if let Some(chip) = found_chip {
                crate::config::registry::get_target_by_chip_info(chip)?
            } else {
                return Err(Error::ChipNotFound(RegistryError::ChipAutodetectFailed));
            }
        }
    };

    Ok((probe.unwrap(), target))
}
