use crate::{
    MemoryInterface,
    architecture::arm::{ArmError, memory::ArmMemoryInterface},
    memory::CoreMemoryInterface,
};

/// Generic handle representing a memory access port on an MCU.
///
/// Some SoCs allow access to system memory via a dedicated memory access port. This structure
/// models such a port. This should be considered as a temporary access to the memory of a system
/// which locks the debug probe driver to as single consumer by borrowing it.
///
/// As soon as you did your atomic task (e.g. read or write memory, for example the RTT buffer) you
/// should drop this object, to allow potential other shareholders of the session struct to grab a
/// core handle or memory access port too.
pub struct MemoryAccessPort<'probe> {
    name: String,
    id: usize,

    memory: Box<dyn MemoryInterface + 'probe>,
}

impl core::fmt::Debug for MemoryAccessPort<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryAccessPort")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

impl<'probe> MemoryAccessPort<'probe> {
    /// Create a new [`MemoryAccessPort`].
    pub fn new_for_core(core: crate::Core<'probe>) -> MemoryAccessPort<'probe> {
        let id = core.id();
        let name = core.name().to_string();
        Self {
            id,
            name,
            memory: Box::new(core),
        }
    }

    /// Create a new [`MemoryAccessPort`] from an [ArmMemoryInterface].
    pub(crate) fn new_for_arm_memory_interface(
        id: usize,
        name: &str,
        memory_port: Box<dyn ArmMemoryInterface + 'probe>,
    ) -> MemoryAccessPort<'probe> {
        Self {
            id,
            name: name.to_string(),
            memory: Box::new(ArmMemoryInterfaceWrapper(memory_port)),
        }
    }
}

impl MemoryAccessPort<'_> {
    /// ID of the memory access port.
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Name of the memory access port.
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct ArmMemoryInterfaceWrapper<'probe>(Box<dyn ArmMemoryInterface + 'probe>);

impl CoreMemoryInterface for ArmMemoryInterfaceWrapper<'_> {
    type ErrorType = ArmError;

    fn memory(&self) -> &dyn MemoryInterface<ArmError> {
        self.0.as_ref()
    }

    fn memory_mut(&mut self) -> &mut dyn MemoryInterface<ArmError> {
        self.0.as_mut()
    }
}

impl CoreMemoryInterface for MemoryAccessPort<'_> {
    type ErrorType = crate::Error;

    fn memory(&self) -> &dyn MemoryInterface {
        self.memory.as_ref()
    }

    fn memory_mut(&mut self) -> &mut dyn MemoryInterface {
        self.memory.as_mut()
    }
}
