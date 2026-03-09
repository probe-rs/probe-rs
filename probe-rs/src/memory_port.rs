use crate::{
    MemoryInterface, Target, architecture::arm::memory::ArmMemoryInterface, rtt::RttAccess,
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
    target: Target,
    is_64_bit: bool,

    memory: Box<dyn MemoryInterface + 'probe>,
}

impl core::fmt::Debug for MemoryAccessPort<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryAccessPort")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("target", &self.target)
            .finish()
    }
}

impl<'probe> MemoryAccessPort<'probe> {
    /// Create a new [`MemoryAccessPort`].
    pub(crate) fn new_for_core(core: crate::Core<'probe>) -> MemoryAccessPort<'probe> {
        let id = core.id();
        let name = core.name().to_string();
        let is_64_bit = core.is_64_bit();
        Self {
            id,
            name,
            target: core.target().clone(),
            is_64_bit,
            memory: Box::new(core),
        }
    }

    /// Create a new [`MemoryAccessPort`] from an [ArmMemoryInterface].
    pub(crate) fn new_for_arm_memory_interface(
        id: usize,
        name: &'probe str,
        target: Target,
        memory_port: Box<dyn ArmMemoryInterface + 'probe>,
        is_64_bit: bool,
    ) -> MemoryAccessPort<'probe> {
        Self {
            id,
            is_64_bit,
            name: name.to_string(),
            target,
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

impl RttAccess for MemoryAccessPort<'_> {
    #[inline]
    fn is_64_bit(&self) -> bool {
        self.is_64_bit
    }

    fn memory_regions(&self) -> impl Iterator<Item = &probe_rs_target::MemoryRegion> {
        self.target.memory_map.iter()
    }
}

pub struct ArmMemoryInterfaceWrapper<'probe>(Box<dyn ArmMemoryInterface + 'probe>);

impl MemoryInterface for ArmMemoryInterfaceWrapper<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.0.supports_native_64bit_access()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        Ok(self.0.read_64(address, data)?)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        Ok(self.0.read_32(address, data)?)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        Ok(self.0.read_16(address, data)?)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        Ok(self.0.read_8(address, data)?)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::Error> {
        Ok(self.0.write_64(address, data)?)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        Ok(self.0.write_32(address, data)?)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        Ok(self.0.write_16(address, data)?)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        Ok(self.0.write_8(address, data)?)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        Ok(self.0.supports_8bit_transfers()?)
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        Ok(self.0.flush()?)
    }
}

impl MemoryInterface for MemoryAccessPort<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.memory.supports_native_64bit_access()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        self.memory.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        self.memory.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        self.memory.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        self.memory.read_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::Error> {
        self.memory.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        self.memory.write_32(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        self.memory.write_16(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.memory.write_8(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        self.memory.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        self.memory.flush()
    }
}
