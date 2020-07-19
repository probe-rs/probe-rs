use crate::architecture::{
    arm::{ArmChipInfo, ArmCommunicationInterface, ArmCommunicationInterfaceState},
    riscv::communication_interface::{
        RiscvCommunicationInterface, RiscvCommunicationInterfaceState,
    },
};
use crate::config::{
    ChipInfo, MemoryRegion, RawFlashAlgorithm, RegistryError, Target, TargetSelector,
};
use crate::core::{Architecture, CoreState, SpecificCoreState};
use crate::{Core, CoreType, Error, Probe};
use anyhow::anyhow;

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
                ArmCommunicationInterface::new(probe, state)?
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
    pub fn new(mut probe: Probe, target: impl Into<TargetSelector>) -> Result<Self, Error> {
        let target = match target.into() {
            TargetSelector::Unspecified(name) => {
                match crate::config::registry::get_target_by_name(name) {
                    Ok(target) => target,
                    Err(err) => return Err(err.into()),
                }
            }
            TargetSelector::Specified(target) => target,
            TargetSelector::Auto => {
                let mut found_chip = None;

                let mut state = ArmCommunicationInterfaceState::new();
                let interface = ArmCommunicationInterface::new(&mut probe, &mut state)?;
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
                    let interface = RiscvCommunicationInterface::new(&mut probe, &mut state)?;

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

        let data = match target.architecture() {
            Architecture::Arm => {
                let state = ArmCommunicationInterfaceState::new();
                (
                    (
                        SpecificCoreState::from_core_type(target.core_type),
                        Core::create_state(),
                    ),
                    ArchitectureInterfaceState::Arm(state),
                )
            }
            Architecture::Riscv => {
                let state = RiscvCommunicationInterfaceState::new();
                (
                    (
                        SpecificCoreState::from_core_type(target.core_type),
                        Core::create_state(),
                    ),
                    ArchitectureInterfaceState::Riscv(state),
                )
            }
        };

        Ok(Self {
            target,
            probe,
            interface_state: data.1,
            cores: vec![data.0],
        })
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
}

fn try_arm_autodetect(
    arm_interface: &mut ArmCommunicationInterface,
) -> Result<Option<ChipInfo>, Error> {
    log::debug!("Autodetect: Trying DAP interface...");

    let found_chip = ArmChipInfo::read_from_rom_table(arm_interface).unwrap_or_else(|e| {
        log::info!("Error during auto-detection of ARM chips: {}", e);
        None
    });

    let found_chip = found_chip.map(ChipInfo::from);

    Ok(found_chip)
}
