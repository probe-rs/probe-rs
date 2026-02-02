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
/// core handle too.
pub struct MemoryAccessPort<'probe> {
    id: usize,
    name: &'probe str,
    target: &'probe Target,
    is_64_bit: bool,

    inner: Inner<'probe>,
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
    #[expect(dead_code)]
    pub(crate) fn new(
        id: usize,
        name: &'probe str,
        target: &'probe Target,
        memory_port: impl MemoryInterface + 'probe,
        is_64_bit: bool,
    ) -> MemoryAccessPort<'probe> {
        Self {
            id,
            is_64_bit,
            name,
            target,
            inner: Inner::Generic(Box::new(memory_port)),
        }
    }

    pub(crate) fn new_for_arm(
        id: usize,
        name: &'probe str,
        target: &'probe Target,
        memory_port: Box<dyn ArmMemoryInterface + 'probe>,
        is_64_bit: bool,
    ) -> MemoryAccessPort<'probe> {
        Self {
            id,
            name,
            is_64_bit,
            target,
            inner: Inner::Arm(memory_port),
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
        self.name
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

enum Inner<'probe> {
    Arm(Box<dyn ArmMemoryInterface + 'probe>),
    Generic(Box<dyn MemoryInterface + 'probe>),
}

impl MemoryInterface for Inner<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        match self {
            Inner::Arm(arm_memory_interface) => arm_memory_interface.supports_native_64bit_access(),
            Inner::Generic(memory_interface) => memory_interface.supports_native_64bit_access(),
        }
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.read_64(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.read_64(address, data),
        }
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.read_32(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.read_32(address, data),
        }
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.read_16(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.read_16(address, data),
        }
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.read_8(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.read_8(address, data),
        }
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.write_64(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.write_64(address, data),
        }
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.write_32(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.write_32(address, data),
        }
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.write_16(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.write_16(address, data),
        }
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.write_8(address, data)?),
            Inner::Generic(memory_interface) => memory_interface.write_8(address, data),
        }
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.supports_8bit_transfers()?),
            Inner::Generic(memory_interface) => memory_interface.supports_8bit_transfers(),
        }
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        match self {
            Inner::Arm(arm_memory_interface) => Ok(arm_memory_interface.flush()?),
            Inner::Generic(memory_interface) => memory_interface.flush(),
        }
    }
}

impl MemoryInterface for MemoryAccessPort<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.inner.supports_native_64bit_access()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        self.inner.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        self.inner.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        self.inner.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        self.inner.read_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::Error> {
        self.inner.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        self.inner.write_32(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        self.inner.write_16(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.inner.write_8(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        self.inner.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        self.inner.flush()
    }
}
