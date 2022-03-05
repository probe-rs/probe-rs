use crate::architecture::arm::{
    ap::{AccessPort, MemoryAp},
    memory::adi_v5_memory_interface::ArmProbe,
    ApAddress,
};
use crate::{
    architecture::arm::{communication_interface::Initialized, ArmCommunicationInterface},
    error,
};

use anyhow::anyhow;
use anyhow::Result;

/// An interface to be implemented for drivers that allow target memory access.
pub trait MemoryInterface {
    /// Does this interface support native 64-bit wide accesses
    ///
    /// If false all 64-bit operations may be split into 32 or 8 bit operations.
    /// Most callers will not need to pivot on this but it can be useful for
    /// picking the fastest bulk data transfer method.
    fn supports_native_64bit_access(&mut self) -> bool;

    /// Read a 64bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_word_64(&mut self, address: u64) -> Result<u64, error::Error>;

    /// Read a 32bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_word_32(&mut self, address: u64) -> Result<u32, error::Error>;

    /// Read an 8bit word of at `address`.
    fn read_word_8(&mut self, address: u64) -> Result<u8, error::Error>;

    /// Read a block of 64bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), error::Error>;

    /// Read a block of 32bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), error::Error>;

    /// Read a block of 8bit words at `address`.
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error>;

    /// Reads bytes using 64 bit memory access. Address must be 64 bit aligned
    /// and data must be an exact multiple of 8.
    fn read_mem_64bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error> {
        // Default implementation uses `read_64`, then converts u64 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 8 != 0 {
            return Err(error::Error::Other(anyhow!(
                "Call to read_mem_64bit with data.len() not a multiple of 8"
            )));
        }
        let mut buffer = vec![0u64; data.len() / 8];
        self.read_64(address, &mut buffer)?;
        for (bytes, value) in data.chunks_exact_mut(8).zip(buffer.iter()) {
            bytes.copy_from_slice(&u64::to_le_bytes(*value));
        }
        Ok(())
    }

    /// Reads bytes using 32 bit memory access. Address must be 32 bit aligned
    /// and data must be an exact multiple of 4.
    fn read_mem_32bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error> {
        // Default implementation uses `read_32`, then converts u32 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 4 != 0 {
            return Err(error::Error::Other(anyhow!(
                "Call to read_mem_32bit with data.len() not a multiple of 4"
            )));
        }
        let mut buffer = vec![0u32; data.len() / 4];
        self.read_32(address, &mut buffer)?;
        for (bytes, value) in data.chunks_exact_mut(4).zip(buffer.iter()) {
            bytes.copy_from_slice(&u32::to_le_bytes(*value));
        }
        Ok(())
    }

    /// Read a block of 8bit words at `address`. May use 32 bit memory access,
    /// so should only be used if reading memory locations that don't have side
    /// effects. Generally faster than `read_8`.
    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error> {
        if self.supports_native_64bit_access() && address % 8 == 0 && data.len() % 8 == 0 {
            // Avoid heap allocation and copy if we don't need it.
            self.read_mem_64bit(address, data)?;
        } else if address % 4 == 0 && data.len() % 4 == 0 {
            // Avoid heap allocation and copy if we don't need it.
            self.read_mem_32bit(address, data)?;
        } else {
            let start_extra_count = (address % 4) as usize;
            let mut buffer = vec![0u8; (start_extra_count + data.len() + 3) / 4 * 4];
            self.read_mem_32bit(address - start_extra_count as u64, &mut buffer)?;
            data.copy_from_slice(&buffer[start_extra_count..start_extra_count + data.len()]);
        }
        Ok(())
    }

    /// Write a 64bit word at `address`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), error::Error>;

    /// Write a 32bit word at `address`.
    ///
    /// The address where the write should be performed at has tgio be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), error::Error>;

    /// Write an 8bit word at `address`.
    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), error::Error>;

    /// Write a block of 64bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), error::Error>;

    /// Write a block of 32bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), error::Error>;

    /// Write a block of 8bit words at `address`.
    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), error::Error>;

    /// Flush any outstanding operations.
    ///
    /// For performance, debug probe implementations may choose to batch writes;
    /// to assure that any such batched writes have in fact been issued, `flush`
    /// can be called.  Takes no arguments, but may return failure if a batched
    /// operation fails.
    fn flush(&mut self) -> Result<(), error::Error>;
}

impl<T> MemoryInterface for &mut T
where
    T: MemoryInterface,
{
    fn supports_native_64bit_access(&mut self) -> bool {
        (*self).supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, error::Error> {
        (*self).read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, error::Error> {
        (*self).read_word_32(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, error::Error> {
        (*self).read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), error::Error> {
        (*self).read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), error::Error> {
        (*self).read_32(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error> {
        (*self).read_8(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), error::Error> {
        (*self).write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), error::Error> {
        (*self).write_word_32(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), error::Error> {
        (*self).write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), error::Error> {
        (*self).write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), error::Error> {
        (*self).write_32(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), error::Error> {
        (*self).write_8(address, data)
    }

    fn flush(&mut self) -> Result<(), error::Error> {
        (*self).flush()
    }
}

/// A struct to allow memory access via an ARM probe.
pub struct Memory<'probe> {
    inner: Box<dyn ArmProbe + 'probe>,
    ap_sel: MemoryAp,
}

impl<'probe> Memory<'probe> {
    /// Constructs a new [`Memory`] handle with a ARM probe and a memory AP.
    pub fn new(memory: impl ArmProbe + 'probe + Sized, ap_sel: MemoryAp) -> Memory<'probe> {
        Self {
            inner: Box::new(memory),
            ap_sel,
        }
    }

    /// Does this interface support native 64-bit wide accesses
    pub fn supports_native_64bit_access(&mut self) -> bool {
        self.inner.supports_native_64bit_access()
    }

    /// Reads a 64 bit word from `address`.
    pub fn read_word_64(&mut self, address: u64) -> Result<u64, error::Error> {
        let mut buff = [0];
        self.inner.read_64(self.ap_sel, address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads a 32 bit word from `address`.
    pub fn read_word_32(&mut self, address: u64) -> Result<u32, error::Error> {
        let mut buff = [0];
        self.inner.read_32(self.ap_sel, address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads an 8 bit word from `address`.
    pub fn read_word_8(&mut self, address: u64) -> Result<u8, error::Error> {
        let mut buff = [0];
        self.inner.read_8(self.ap_sel, address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads `data.len()` 64 bit words from `address` into `data`.
    pub fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), error::Error> {
        self.inner.read_64(self.ap_sel, address, data)
    }

    /// Reads `data.len()` 32 bit words from `address` into `data`.
    pub fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), error::Error> {
        self.inner.read_32(self.ap_sel, address, data)
    }

    /// Reads `data.len()` 8 bit words from `address` into `data`.
    pub fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), error::Error> {
        self.inner.read_8(self.ap_sel, address, data)
    }

    /// Writes a 64 bit word to `address`.
    pub fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), error::Error> {
        self.inner.write_64(self.ap_sel, address, &[data])
    }

    /// Writes a 32 bit word to `address`.
    pub fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), error::Error> {
        self.inner.write_32(self.ap_sel, address, &[data])
    }

    /// Writes a 8 bit word to `address`.
    pub fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), error::Error> {
        self.inner.write_8(self.ap_sel, address, &[data])
    }

    /// Writes `data.len()` 32 bit words from `data` to `address`.
    pub fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), error::Error> {
        self.inner.write_64(self.ap_sel, address, data)
    }

    /// Writes `data.len()` 32 bit words from `data` to `address`.
    pub fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), error::Error> {
        self.inner.write_32(self.ap_sel, address, data)
    }

    /// Writes `data.len()` 8 bit words from `data` to `address`.
    pub fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), error::Error> {
        self.inner.write_8(self.ap_sel, address, data)
    }

    /// Flushes all pending writes to the target.
    ///
    /// This method is necessary when the underlying probe driver implements batching.
    pub fn flush(&mut self) -> Result<(), error::Error> {
        self.inner.flush()
    }

    /// Tries to borrow the underlying [`ArmCommunicationInterface`].
    pub fn get_arm_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, error::Error> {
        self.inner.get_arm_communication_interface()
    }

    /// Borrows the underlying [`ArmProbe`] driver.
    pub fn get_arm_probe(&mut self) -> &mut dyn ArmProbe {
        self.inner.as_mut()
    }

    /// Returns the underlying [`ApAddress`].
    pub fn get_ap(&mut self) -> ApAddress {
        self.ap_sel.ap_address()
    }
}

// Helper functions to validate address space constraints

/// Validate that an input address is valid for 32-bit only systems
pub(crate) fn valid_32_address(address: u64) -> Result<u32, error::Error> {
    let address: u32 = address
        .try_into()
        .map_err(|_| anyhow!("Address {:#08x} out of range", address))?;

    Ok(address)
}
