use crate::architecture::{
    arm::{
        ap::MemoryAP,
        communication_interface::ApInformation::{MemoryAp, Other},
        core::{debug_core_start, reset_catch_clear, reset_catch_set},
        memory::Component,
        ArmChipInfo, ArmCommunicationInterface, ArmCommunicationInterfaceState, SwoAccess,
        SwoConfig,
    },
    riscv::communication_interface::{
        RiscvCommunicationInterface, RiscvCommunicationInterfaceState,
    },
};
use crate::config::{
    ChipInfo, MemoryRegion, RawFlashAlgorithm, RegistryError, Target, TargetSelector,
};
use crate::core::{Architecture, CoreState, SpecificCoreState};
use crate::{AttachMethod, Core, CoreType, Error, Probe};
use anyhow::anyhow;
use std::time::Duration;

#[derive(Debug)]
pub struct Session {
    target: Target,
    probe: Probe,
    interface_state: ArchitectureInterfaceState,
    cores: Vec<(SpecificCoreState, CoreState)>,
}

#[derive(Debug)]
pub enum ArchitectureInterfaceState {
    Arm(ArmCommunicationInterfaceState),
    Riscv(RiscvCommunicationInterfaceState),
}

impl From<ArchitectureInterfaceState> for Architecture {
    fn from(value: ArchitectureInterfaceState) -> Self {
        match value {
            ArchitectureInterfaceState::Arm(_) => Architecture::Arm,
            ArchitectureInterfaceState::Riscv(_) => Architecture::Riscv,
        }
    }
}

impl ArchitectureInterfaceState {
    fn attach<'probe>(
        &'probe mut self,
        probe: &'probe mut Probe,
        core: &'probe mut SpecificCoreState,
        core_state: &'probe mut CoreState,
    ) -> Result<Core<'probe>, Error> {
        match self {
            ArchitectureInterfaceState::Arm(state) => core.attach_arm(
                core_state,
                probe
                    .get_arm_interface(state)?
                    .ok_or_else(|| anyhow!("No DAP interface available on probe"))?,
            ),
            ArchitectureInterfaceState::Riscv(state) => core.attach_riscv(
                core_state,
                RiscvCommunicationInterface::new(probe, state)?
                    .ok_or_else(|| anyhow!("No JTAG interface available on probe"))?,
            ),
        }
    }
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(
        mut probe: Probe,
        target: impl Into<TargetSelector>,
        attach_method: AttachMethod,
    ) -> Result<Self, Error> {
        let target = get_target_from_selector(target, &mut probe)?;

        let mut session = match target.architecture() {
            Architecture::Arm => {
                let state = ArmCommunicationInterfaceState::new();
                let core = (
                    SpecificCoreState::from_core_type(target.core_type),
                    Core::create_state(0),
                );

                let mut session = Session {
                    target,
                    probe,
                    interface_state: ArchitectureInterfaceState::Arm(state),
                    cores: vec![core],
                };

                // Enable debug mode
                debug_core_start(&mut session.core(0)?)?;

                if attach_method == AttachMethod::UnderReset {
                    // we need to halt the chip here
                    reset_catch_set(&mut session.core(0)?)?;

                    // Deassert the reset pin
                    session.probe.target_reset_deassert()?;

                    // Wait for the core to be halted
                    let mut core = session.core(0)?;

                    core.wait_for_core_halted(Duration::from_millis(100))?;

                    reset_catch_clear(&mut core)?;
                }

                session
            }
            Architecture::Riscv => {
                // TODO: Handle attach under reset

                let state = RiscvCommunicationInterfaceState::new();

                let core = (
                    SpecificCoreState::from_core_type(target.core_type),
                    Core::create_state(0),
                );

                let mut session = Session {
                    target,
                    probe,
                    interface_state: ArchitectureInterfaceState::Riscv(state),
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
        let (core, core_state) = self
            .cores
            .get_mut(n)
            .ok_or_else(|| Error::CoreNotFound(n))?;

        self.interface_state
            .attach(&mut self.probe, core, core_state)
    }

    /// Returns a list of the flash algotithms on the target.
    pub(crate) fn flash_algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.target.flash_algorithms
    }

    pub fn read_swo(&mut self) -> Result<Vec<u8>, Error> {
        let mut interface = self.get_arm_interface()?;
        interface.read_swo()
    }

    pub fn get_arm_interface(&mut self) -> Result<ArmCommunicationInterface, Error> {
        let state = match &mut self.interface_state {
            ArchitectureInterfaceState::Arm(state) => state,
            _ => return Err(Error::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        };

        Ok(self.probe.get_arm_interface(state)?.unwrap())
    }

    pub fn get_arm_component(&mut self) -> Result<Component, Error> {
        let interface = self.get_arm_interface()?;

        let ap_index = MemoryAP::from(0);

        let ap_information = interface
            .ap_information(ap_index)
            .ok_or_else(|| anyhow!("AP {:?} does not exist on chip.", ap_index))?;

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
        let mut interface = self.get_arm_interface()?;
        interface.enable_swo(config)?;

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
        match self.interface_state {
            ArchitectureInterfaceState::Arm(_) => Architecture::Arm,
            ArchitectureInterfaceState::Riscv(_) => Architecture::Riscv,
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

fn try_arm_autodetect(
    arm_interface: &mut ArmCommunicationInterface,
) -> Result<Option<ChipInfo>, Error> {
    log::debug!("Autodetect: Trying DAP interface...");

    let found_chip = ArmChipInfo::read_from_rom_table(arm_interface).unwrap_or_else(|e| {
        log::info!("Error during auto-detection of ARM chips: {}", e);
        None
    });

    if let Some(found_chip) = &found_chip {
        log::debug!("Autodect: Found information {:?}", found_chip);
    }

    let found_chip = found_chip.map(ChipInfo::from);

    Ok(found_chip)
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
    probe: &mut Probe,
) -> Result<Target, Error> {
    let target = match target.into() {
        TargetSelector::Unspecified(name) => crate::config::registry::get_target_by_name(name)?,
        TargetSelector::Specified(target) => target,
        TargetSelector::Auto => {
            let mut found_chip = None;

            let mut state = ArmCommunicationInterfaceState::new();
            let interface = probe.get_arm_interface(&mut state)?;
            if let Some(mut interface) = interface {
                let chip_result = try_arm_autodetect(&mut interface);

                // Ignore errors during autodetect
                found_chip = chip_result.unwrap_or_else(|e| {
                    log::debug!("An error occured during ARM autodetect: {}", e);
                    None
                });
            } else {
                log::debug!("No DAP interface was present. This is not an ARM core. Skipping ARM autodetect.");
            }

            if found_chip.is_none() && probe.has_jtag_interface() {
                let mut state = RiscvCommunicationInterfaceState::new();
                let interface = RiscvCommunicationInterface::new(probe, &mut state)?;

                if let Some(mut interface) = interface {
                    let idcode = interface.read_idcode();

                    log::debug!("ID Code read over JTAG: {:x?}", idcode);
                } else {
                    log::debug!("No JTAG interface was present. Skipping Riscv autodetect.");
                }

                // TODO: Implement autodetect for RISC-V
            }

            if let Some(chip) = found_chip {
                crate::config::registry::get_target_by_chip_info(chip)?
            } else {
                return Err(Error::ChipNotFound(RegistryError::ChipAutodetectFailed));
            }
        }
    };

    Ok(target)
}
