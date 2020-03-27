use crate::error;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

pub trait MemoryInterface {
    /// Read a 32bit word of at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read32(&mut self, address: u32) -> Result<u32, error::Error>;

    /// Read an 8bit word of at `addr`.
    fn read8(&mut self, address: u32) -> Result<u8, error::Error>;

    /// Read a block of 32bit words at `addr`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), error::Error>;

    /// Read a block of 8bit words at `addr`.
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), error::Error>;

    /// Write a 32bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write32(&mut self, addr: u32, data: u32) -> Result<(), error::Error>;

    /// Write an 8bit word at `addr`.
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), error::Error>;

    /// Write a block of 32bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), error::Error>;

    /// Write a block of 8bit words at `addr`.
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), error::Error>;
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

impl MemoryInterface for MemoryDummy {
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

#[derive(Clone)]
pub struct Memory {
    inner: Rc<RefCell<dyn MemoryInterface>>,
}

impl Memory {
    pub fn new(memory: impl MemoryInterface + 'static) -> Self {
        Self {
            inner: Rc::new(RefCell::new(memory)),
        }
    }

    pub fn new_dummy() -> Self {
        Self::new(MemoryDummy)
    }

    pub fn memory_interface(&self) -> Ref<dyn MemoryInterface> {
        self.inner.borrow()
    }

    pub fn memory_interface_mut(&mut self) -> RefMut<dyn MemoryInterface> {
        self.inner.borrow_mut()
    }

    pub fn read32(&self, address: u32) -> Result<u32, error::Error> {
        self.inner.borrow_mut().read32(address)
    }

    pub fn read8(&self, address: u32) -> Result<u8, error::Error> {
        self.inner.borrow_mut().read8(address)
    }

    pub fn read_block32(&self, address: u32, data: &mut [u32]) -> Result<(), error::Error> {
        self.inner.borrow_mut().read_block32(address, data)
    }

    pub fn read_block8(&self, address: u32, data: &mut [u8]) -> Result<(), error::Error> {
        self.inner.borrow_mut().read_block8(address, data)
    }

    pub fn write32(&self, addr: u32, data: u32) -> Result<(), error::Error> {
        self.inner.borrow_mut().write32(addr, data)
    }

    pub fn write8(&self, addr: u32, data: u8) -> Result<(), error::Error> {
        self.inner.borrow_mut().write8(addr, data)
    }

    pub fn write_block32(&self, addr: u32, data: &[u32]) -> Result<(), error::Error> {
        self.inner.borrow_mut().write_block32(addr, data)
    }

    pub fn write_block8(&self, addr: u32, data: &[u8]) -> Result<(), error::Error> {
        self.inner.borrow_mut().write_block8(addr, data)
    }
}

pub struct MemoryList(Vec<Memory>);

impl MemoryList {
    pub fn new(memories: Vec<Memory>) -> Self {
        Self(memories)
    }
}

impl std::ops::Deref for MemoryList {
    type Target = Vec<Memory>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
