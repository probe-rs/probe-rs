pub mod adi_v5_memory_interface;

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
    type Error;

    /// Read a word of the size defined by S at `addr`.
    /// 
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read<S: ToMemoryReadSize>(&mut self, address: u32) -> Result<S, Self::Error>;

    /// Read a block of words of the size defined by S at `addr`.
    /// 
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_block<S: ToMemoryReadSize>(
        &mut self,
        address: u32,
        data: &mut [S]
    ) -> Result<(), Self::Error>;

    /// Write a word of the size defined by S at `addr`.
    /// 
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), Self::Error>;

    /// Like `write_block` but with much simpler stucture but way lower performance for u8 and u16.
    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), Self::Error>;
}