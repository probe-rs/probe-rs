pub mod adi_v5_memory_interface;
pub mod romtable;
pub mod flash_writer;


use coresight::access_ports::AccessPortError;


pub trait ToMemoryReadSize: Into<u32> + Copy {
    /// The alignment mask that is required to test for properly aligned memory.
    const ALIGNMENT_MASK: u32;
    /// The transfer size expressed in bytes.
    const MEMORY_TRANSFER_SIZE: u8;
    /// Transform a generic 32 bit sized value to a transfer size sized one.
    fn to_result(value: u32) -> Self;
}

impl ToMemoryReadSize for u32 {
    const ALIGNMENT_MASK: u32 = 0x3;
    const MEMORY_TRANSFER_SIZE: u8 = 4;

    fn to_result(value: u32) -> Self {
        value
    }
}

impl ToMemoryReadSize for u16 {
    const ALIGNMENT_MASK: u32 = 0x1;
    const MEMORY_TRANSFER_SIZE: u8 = 2;

    fn to_result(value: u32) -> Self {
        value as u16
    }
}

impl ToMemoryReadSize for u8 {
    const ALIGNMENT_MASK: u32 = 0x0;
    const MEMORY_TRANSFER_SIZE: u8 = 1;

    fn to_result(value: u32) -> Self {
        value as u8
    }
}

pub trait MI {
    /// Read a 32bit word of at `addr`.
    /// 
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read32(&mut self, address: u32) -> Result<u32, AccessPortError>;

    /// Read an 8bit word of at `addr`.
    /// 
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read8(&mut self, address: u32) -> Result<u8, AccessPortError>;

    /// Read a block of 32bit words at `addr`.
    /// 
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_block32(
        &mut self,
        address: u32,
        data: &mut [u32]
    ) -> Result<(), AccessPortError>;

    /// Read a block of 8bit words at `addr`.
    /// 
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_block8(
        &mut self,
        address: u32,
        data: &mut [u8]
    ) -> Result<(), AccessPortError>;

    /// Write a word of the size defined by S at `addr`.
    /// 
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError>;

    /// Like `write_block` but with much simpler stucture but way lower performance for u8 and u16.
    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError>;
}

impl<T> MI for &mut T where T: MI {
    fn read32(&mut self, address: u32) -> Result<u32, AccessPortError> {
        (*self).read32(address)
    }

    fn read8(&mut self, address: u32) -> Result<u8, AccessPortError> {
        (*self).read8(address)
    }

    fn read_block32(
        &mut self,
        address: u32,
        data: &mut [u32]
    ) -> Result<(), AccessPortError> {
        (*self).read_block32(address, data)
    }

    fn read_block8(
        &mut self,
        address: u32,
        data: &mut [u8]
    ) -> Result<(), AccessPortError> {
        (*self).read_block8(address, data)
    }

    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError> {
        (*self).write(addr, data)
    }

    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
        (*self).write_block(addr, data)
    }
}
