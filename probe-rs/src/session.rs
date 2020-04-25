use crate::architecture::{
    arm::{
        memory::romtable::Component, memory::ADIMemoryInterface, ArmChipInfo,
        ArmCommunicationInterface,
    },
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::config::{
    ChipInfo, MemoryRegion, RawFlashAlgorithm, RegistryError, Target, TargetSelector,
};
use crate::core::Architecture;
use crate::{Core, CoreList, Error, Memory, MemoryList, Probe};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct Session {
    inner: Rc<RefCell<InnerSession>>,
}

struct InnerSession {
    target: Target,
    architecture_session: ArchitectureSession,
}

enum ArchitectureSession {
    Arm(ArmCommunicationInterface),
    Riscv(RiscvCommunicationInterface),
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(probe: Probe, target: impl Into<TargetSelector>) -> Result<Self, Error> {
        let mut generic_probe = Some(probe);

        let target = match target.into() {
            TargetSelector::Unspecified(name) => {
                match crate::config::registry::get_target_by_name(name) {
                    Ok(target) => target,
                    Err(err) => return Err(err.into()),
                }
            }
            TargetSelector::Specified(target) => target,
            TargetSelector::Auto => {
                let (returned_probe, found_chip) =
                    try_arm_autodetect(generic_probe.take().unwrap());

                // Ignore errors during autodetect
                let found_chip = found_chip.unwrap_or_else(|e| {
                    log::debug!("Error during autodetect: {}", e);
                    None
                });

                generic_probe = Some(returned_probe);

                if found_chip.is_none() && generic_probe.as_ref().unwrap().has_jtag_interface() {
                    let riscv_interface =
                        RiscvCommunicationInterface::new(generic_probe.take().unwrap())?;

                    let idcode = riscv_interface.read_idcode();

                    log::debug!("ID Code read over JTAG: {:x?}", idcode);

                    // TODO: Implement autodetect for RISC-V

                    // This will always work, the interface is created and used only in this function
                    generic_probe = Some(riscv_interface.close().unwrap());
                }

                if let Some(chip) = found_chip {
                    crate::config::registry::get_target_by_chip_info(chip)?
                } else {
                    // Not sure if this is ok.
                    return Err(Error::ChipNotFound(RegistryError::ChipAutodetectFailed));
                }
            }
        };

        let session = match target.architecture() {
            Architecture::ARM => {
                let arm_interface = ArmCommunicationInterface::new(generic_probe.unwrap())
                    .map_err(|(_probe, err)| err)?;
                ArchitectureSession::Arm(arm_interface)
            }
            Architecture::RISCV => {
                let riscv_interface = RiscvCommunicationInterface::new(generic_probe.unwrap())?;
                ArchitectureSession::Riscv(riscv_interface)
            }
        };

        Ok(Self {
            inner: Rc::new(RefCell::new(InnerSession {
                target,
                architecture_session: session,
            })),
        })
    }

    pub fn list_cores(&self) -> CoreList {
        CoreList::new(vec![self.inner.borrow().target.core_type])
    }

    pub fn attach_to_core(&self, n: usize) -> Result<Core, Error> {
        let core = *self
            .list_cores()
            .get(n)
            .ok_or_else(|| Error::CoreNotFound(n))?;

        match self.inner.borrow().architecture_session {
            ArchitectureSession::Arm(ref arm_interface) => {
                core.attach_arm(arm_interface.clone(), n)
            }
            ArchitectureSession::Riscv(ref riscv_interface) => {
                core.attach_riscv(riscv_interface.clone(), n)
            }
        }
    }

    pub fn list_memories(&self) -> MemoryList {
        MemoryList::new(vec![])
    }

    pub fn attach_to_memory(&self, id: usize) -> Result<Memory, Error> {
        match self.inner.borrow_mut().architecture_session {
            ArchitectureSession::Arm(ref mut interface) => {
                if let Ok(Some(memory)) = interface.dedicated_memory_interface() {
                    Ok(memory)
                } else {
                    // TODO: Change this to actually grab the proper memory IF.
                    // For now always use the ARM IF.
                    let maps = interface.memory_access_ports()?;
                    Ok(Memory::new(
                        ADIMemoryInterface::<ArmCommunicationInterface>::new(
                            interface.clone(),
                            maps[id].id(),
                        )
                        .map_err(|(_i, e)| Error::architecture_specific(e))?,
                    ))
                }
            }
            ArchitectureSession::Riscv(ref _interface) => {
                // We don't need a memory interface..
                Ok(Memory::new_dummy())
            }
        }
    }

    pub fn flash_algorithms(&self) -> Vec<RawFlashAlgorithm> {
        self.inner.borrow().target.flash_algorithms.clone()
    }

    pub fn memory_map(&self) -> Vec<MemoryRegion> {
        self.inner.borrow().target.memory_map.clone()
    }

    pub fn read_swv(&self) -> Result<Vec<u8>, Error> {
        use crate::architecture::arm::SwvAccess;
        match &mut self.inner.borrow_mut().architecture_session {
            ArchitectureSession::Arm(interface) => interface.read_swv(),
            _ => Err(Error::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    pub fn setup_tracing(&mut self, core: &mut Core) -> Result<(), Error> {
        match self.inner.borrow_mut().architecture_session {
            ArchitectureSession::Arm(ref mut interface) => {
                let maps = interface.memory_access_ports()?;

                let baseaddr = maps[core.id()].base_address();

                let component = Component::try_parse(core, baseaddr as u64)
                    .map_err(Error::architecture_specific)?;

                crate::architecture::arm::component::setup_tracing(core, &component)
            }
            _ => Err(Error::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }

    pub fn trace_enable(&mut self, core: &mut Core) -> Result<(), Error> {
        crate::architecture::arm::component::trace_enable(core)
    }

    pub fn enable_data_trace(
        &mut self,
        core: &mut Core,
        unit: usize,
        address: u32,
    ) -> Result<(), Error> {
        match self.inner.borrow_mut().architecture_session {
            ArchitectureSession::Arm(ref mut interface) => {
                let maps = interface.memory_access_ports()?;

                let baseaddr = maps[core.id()].base_address();

                let component = Component::try_parse(&mut core.memory(), baseaddr as u64)
                    .map_err(Error::architecture_specific)?;

                crate::architecture::arm::component::enable_data_trace(
                    core, &component, unit, address,
                )
            }
            _ => Err(Error::ArchitectureRequired(&["ARMv7", "ARMv8"])),
        }
    }
}

fn try_arm_autodetect(probe: Probe) -> (Probe, Result<Option<ChipInfo>, Error>) {
    if probe.has_dap_interface() {
        log::debug!("Autodetect: Trying DAP interface...");

        let arm_interface = ArmCommunicationInterface::new(probe);

        match arm_interface {
            Ok(arm_interface) => {
                let (arm_interface, found_chip) = ArmChipInfo::read_from_rom_table(arm_interface);
                let found_chip = found_chip.unwrap_or_else(|e| {
                    log::info!("Error during auto-detection of ARM chips: {}", e);
                    None
                });

                // This will always work, the interface is created and used only in this function
                let probe = arm_interface.close().unwrap();

                let found_chip = found_chip.map(ChipInfo::from);

                (probe, Ok(found_chip))
            }
            Err((probe, error)) => (probe, Err(error.into())),
        }
    } else {
        log::debug!("No DAP interface available on Probe");

        (probe, Ok(None))
    }
}

// pub struct Session {
//     probe: Probe,
// }

// pub trait Session {
//     fn get_core(n: usize) -> Result<Core, Error>;
// }

// pub struct ArmSession {
//     pub target: Target,
//     pub probe: Rc<RefCell<dyn DAPAccess>>,
// }

// impl ArmSession {
//     pub fn new(target: Target, probe: impl DAPAccess) -> Self {
//         Self {
//             target,
//             probe: Rc::new(RefCell::new(probe)),
//         }
//     }
// }

// pub struct RiscVSession {
//     pub target: Target,
//     pub probe: Rc<RefCell<dyn DAPAccess>>,
// }

// impl RiscVSession {
//     pub fn new(target: Target, probe: impl DAPAccess) -> Self {
//         Self {
//             target,
//             probe: Rc::new(RefCell::new(probe)),
//         }
//     }
// }

// impl Session for RiscVSession {
//     fn get_core(n: usize) -> Result<Core, Error> {}
// }
