use crate::architecture::arm::{memory::ADIMemoryInterface, ArmCommunicationInterface};
use crate::config::flash_algorithm::RawFlashAlgorithm;
use crate::config::memory::MemoryRegion;
use crate::config::target::Target;
use crate::core::CoreType;
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
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(probe: Probe, target: Target) -> Self {
        // TODO: Handle different architectures

        let session = ArchitectureSession::Arm(ArmCommunicationInterface::new(probe));

        Self {
            inner: Rc::new(RefCell::new(InnerSession {
                target,
                architecture_session: session,
            })),
        }
    }

    pub fn list_cores(&self) -> CoreList {
        CoreList::new(vec![self.inner.borrow().target.core_type.clone()])
    }

    pub fn attach_to_core(&self, n: usize) -> Result<Core, Error> {
        let core = self
            .list_cores()
            .get(n)
            .ok_or_else(|| Error::CoreNotFound(n))?
            .attach(self.clone(), self.attach_to_memory(0)?);
        Ok(core)
    }

    pub fn attach_to_specific_core(&self, core_type: CoreType) -> Result<Core, Error> {
        let core = core_type.attach(self.clone(), self.attach_to_memory(0)?);
        Ok(core)
    }

    pub fn attach_to_core_with_specific_memory(
        &self,
        n: usize,
        memory: Option<Memory>,
    ) -> Result<Core, Error> {
        let core = self
            .list_cores()
            .get(n)
            .ok_or_else(|| Error::CoreNotFound(n))?
            .attach(
                self.clone(),
                match memory {
                    Some(memory) => memory,
                    None => self.attach_to_memory(0)?,
                },
            );
        Ok(core)
    }

    pub fn list_memories(&self) -> MemoryList {
        MemoryList::new(vec![])
    }

    pub fn attach_to_memory(&self, _id: usize) -> Result<Memory, Error> {
        match self.inner.borrow().architecture_session {
            ArchitectureSession::Arm(ref interface) => {
                if let Some(memory) = interface.dedicated_memory_interface() {
                    Ok(memory)
                } else {
                    // TODO: Change this to actually grab the proper memory IF.
                    // For now always use the ARM IF.
                    Ok(Memory::new(
                        ADIMemoryInterface::<ArmCommunicationInterface>::new(interface.clone(), 0),
                    ))
                }
            }
        }
    }

    pub fn attach_to_best_memory(&self) -> Result<Memory, Error> {
        match self.inner.borrow().architecture_session {
            ArchitectureSession::Arm(ref interface) => {
                if let Some(memory) = interface.dedicated_memory_interface() {
                    Ok(memory)
                } else {
                    // TODO: Change this to actually grab the proper memory IF.
                    // For now always use the ARM IF.
                    Ok(Memory::new(
                        ADIMemoryInterface::<ArmCommunicationInterface>::new(interface.clone(), 0),
                    ))
                }
            }
        }
    }

    pub fn flash_algorithms(&self) -> Vec<RawFlashAlgorithm> {
        self.inner.borrow().target.flash_algorithms.clone()
    }

    pub fn memory_map(&self) -> Vec<MemoryRegion> {
        self.inner.borrow().target.memory_map.clone()
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
