use crate::error;

pub trait MemoryInterface {
    /// Read a 32bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read32(&mut self, address: u32) -> Result<u32, error::Error>;

    /// Read an 8bit word of at `address`.
    fn read8(&mut self, address: u32) -> Result<u8, error::Error>;

    /// Read a block of 32bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), error::Error>;

    /// Read a block of 8bit words at `address`.
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), error::Error>;

    /// Write a 32bit word at `address`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write32(&mut self, address: u32, data: u32) -> Result<(), error::Error>;

    /// Write an 8bit word at `address`.
    fn write8(&mut self, address: u32, data: u8) -> Result<(), error::Error>;

    /// Write a block of 32bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_block32(&mut self, address: u32, data: &[u32]) -> Result<(), error::Error>;

    /// Write a block of 8bit words at `address`.
    fn write_block8(&mut self, address: u32, data: &[u8]) -> Result<(), error::Error>;
}

impl<T> MemoryInterface for &mut T
where
    T: MemoryInterface,
{
    fn read32(&mut self, address: u32) -> Result<u32, error::Error> {
        (*self).read32(address)
    }

    fn read8(&mut self, address: u32) -> Result<u8, error::Error> {
        (*self).read8(address)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), error::Error> {
        (*self).read_block32(address, data)
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), error::Error> {
        (*self).read_block8(address, data)
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<(), error::Error> {
        (*self).write32(addr, data)
    }

    fn write8(&mut self, addr: u32, data: u8) -> Result<(), error::Error> {
        (*self).write8(addr, data)
    }

    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), error::Error> {
        (*self).write_block32(addr, data)
    }

    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), error::Error> {
        (*self).write_block8(addr, data)
    }
}

pub struct MemoryDummy;

impl<'a> MemoryInterface for MemoryDummy {
    fn read32(&mut self, _address: u32) -> Result<u32, error::Error> {
        unimplemented!()
    }
    fn read8(&mut self, _address: u32) -> Result<u8, error::Error> {
        unimplemented!()
    }
    fn read_block32(&mut self, _address: u32, _data: &mut [u32]) -> Result<(), error::Error> {
        unimplemented!()
    }
    fn read_block8(&mut self, _address: u32, _data: &mut [u8]) -> Result<(), error::Error> {
        unimplemented!()
    }
    fn write32(&mut self, _address: u32, _data: u32) -> Result<(), error::Error> {
        unimplemented!()
    }
    fn write8(&mut self, _address: u32, _data: u8) -> Result<(), error::Error> {
        unimplemented!()
    }
    fn write_block32(&mut self, _address: u32, _data: &[u32]) -> Result<(), error::Error> {
        unimplemented!()
    }
    fn write_block8(&mut self, _address: u32, _data: &[u8]) -> Result<(), error::Error> {
        unimplemented!()
    }
}

pub struct Memory<'a> {
    inner: Box<dyn MemoryInterface + 'a>,
}

impl<'a> Memory<'a> {
    pub fn new(memory: impl MemoryInterface + 'a + Sized) -> Memory<'a> {
        Self {
            inner: Box::new(memory),
        }
    }

    pub fn new_dummy() -> Self {
        Self::new(MemoryDummy)
    }

    pub fn memory_interface(&self) -> &dyn MemoryInterface {
        self.inner.as_ref()
    }

    pub fn memory_interface_mut<'b>(&'b mut self) -> &mut dyn MemoryInterface {
        self.inner.as_mut()
    }

    pub fn read32<'b>(&'b mut self, address: u32) -> Result<u32, error::Error> {
        self.inner.read32(address)
    }

    pub fn read8<'b>(&'b mut self, address: u32) -> Result<u8, error::Error> {
        self.inner.read8(address)
    }

    pub fn read_block32<'b>(
        &'b mut self,
        address: u32,
        data: &mut [u32],
    ) -> Result<(), error::Error> {
        self.inner.read_block32(address, data)
    }

    pub fn read_block8<'b>(
        &'b mut self,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), error::Error> {
        self.inner.read_block8(address, data)
    }

    pub fn write32<'b>(&'b mut self, addr: u32, data: u32) -> Result<(), error::Error> {
        self.inner.write32(addr, data)
    }

    pub fn write8<'b>(&'b mut self, addr: u32, data: u8) -> Result<(), error::Error> {
        self.inner.write8(addr, data)
    }

    pub fn write_block32<'b>(&'b mut self, addr: u32, data: &[u32]) -> Result<(), error::Error> {
        self.inner.write_block32(addr, data)
    }

    pub fn write_block8<'b>(&'b mut self, addr: u32, data: &[u8]) -> Result<(), error::Error> {
        self.inner.write_block8(addr, data)
    }
}

pub struct MemoryList<'a>(Vec<Memory<'a>>);

impl<'a> MemoryList<'a> {
    pub fn new(memories: Vec<Memory<'a>>) -> Self {
        Self(memories)
    }
}

impl<'a> std::ops::Deref for MemoryList<'a> {
    type Target = Vec<Memory<'a>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
