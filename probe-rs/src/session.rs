use crate::architecture::{
    arm::{ArmChipInfo, ArmCommunicationInterface, ArmCommunicationInterfaceState},
    riscv::communication_interface::{
        RiscvCommunicationInterface, RiscvCommunicationInterfaceState,
    },
};
use crate::config::{
    ChipInfo, MemoryRegion, RawFlashAlgorithm, RegistryError, Target, TargetSelector,
};
use crate::core::{Architecture, CoreState};
use crate::{Core, CoreType, DebugProbeError, Error, Probe};

pub struct Session {
    target: Target,
    probe: Probe,
    cores: Vec<(CoreType, CoreState, ArchitectureState)>,
}

pub enum ArchitectureState {
    Arm(ArmCommunicationInterfaceState),
    Riscv(RiscvCommunicationInterfaceState),
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

                let state = &mut ArmCommunicationInterface::create_state(&mut probe)?;
                let interface = ArmCommunicationInterface::new(&mut probe, state)?;
                if let Some(interface) = interface {
                    let chip_result = try_arm_autodetect(interface);

                    // Ignore errors during autodetect
                    found_chip = chip_result.unwrap_or_else(|e| {
                        log::debug!("An error occured during ARM autodetect: {}", e);
                        None
                    });
                } else {
                    log::debug!("No DAP interface was present. This is not an ARM core. Skipping ARM autodetect.");
                }

                if found_chip.is_none() && probe.has_jtag_interface() {
                    let state = &mut RiscvCommunicationInterface::create_state(&mut probe)?;
                    let interface = RiscvCommunicationInterface::new(&mut probe, state)?;

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

        let core = match target.architecture() {
            Architecture::ARM => {
                let arm_interface = ArmCommunicationInterface::create_state(&mut probe)?;
                (
                    target.core_type,
                    Core::create_state(),
                    ArchitectureState::Arm(arm_interface),
                )
            }
            Architecture::RISCV => {
                let riscv_interface = RiscvCommunicationInterface::create_state(&mut probe)?;
                (
                    target.core_type,
                    Core::create_state(),
                    ArchitectureState::Riscv(riscv_interface),
                )
            }
            _ => unimplemented!(),
        };

        Ok(Self {
            target,
            probe,
            cores: vec![core],
        })
    }

    pub fn list_cores<'a>(&'a self) -> &'a Vec<(CoreType, CoreState, ArchitectureState)> {
        &self.cores
    }

    pub fn list_cores_mut<'a>(
        &'a mut self,
    ) -> &'a mut Vec<(CoreType, CoreState, ArchitectureState)> {
        &mut self.cores
    }

    pub fn attach_to_core<'a: 'p, 'p>(&'a mut self, n: usize) -> Result<Core<'p>, Error> {
        let (core, core_state, architecture_state) = self
            .cores
            .get_mut(n)
            .ok_or_else(|| Error::CoreNotFound(n))?;

        match architecture_state {
            ArchitectureState::Arm(architecture_state) => core.attach_arm(
                core_state,
                ArmCommunicationInterface::new(&mut self.probe, architecture_state)?
                    .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("DAP"))?,
            ),
            ArchitectureState::Riscv(architecture_state) => core.attach_riscv(
                core_state,
                RiscvCommunicationInterface::new(&mut self.probe, architecture_state)?
                    .ok_or_else(|| DebugProbeError::InterfaceNotAvailable("DAP"))?,
            ),
        }
    }

    pub fn flash_algorithms(&self) -> &Vec<RawFlashAlgorithm> {
        &self.target.flash_algorithms
    }

    pub fn memory_map(&self) -> &Vec<MemoryRegion> {
        &self.target.memory_map
    }
}

fn try_arm_autodetect(arm_interface: ArmCommunicationInterface) -> Result<Option<ChipInfo>, Error> {
    log::debug!("Autodetect: Trying DAP interface...");

    let found_chip = ArmChipInfo::read_from_rom_table(arm_interface).unwrap_or_else(|e| {
        log::info!("Error during auto-detection of ARM chips: {}", e);
        None
    });

    let found_chip = found_chip.map(ChipInfo::from);

    Ok(found_chip)
}
