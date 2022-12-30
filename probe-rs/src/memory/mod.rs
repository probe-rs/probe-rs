use crate::error::Error;

use anyhow::{anyhow, Result};
use scroll::Pread;

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
    fn read_word_64(&mut self, address: u64) -> Result<u64, Error>;

    /// Read a 32bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn read_word_32(&mut self, address: u64) -> Result<u32, Error>;

    /// Read an 8bit word of at `address`.
    fn read_word_8(&mut self, address: u64) -> Result<u8, Error>;

    /// Read a block of 64bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error>;

    /// Read a block of 32bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error>;

    /// Read a block of 8bit words at `address`.
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error>;

    /// Reads bytes using 64 bit memory access. Address must be 64 bit aligned
    /// and data must be an exact multiple of 8.
    fn read_mem_64bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        // Default implementation uses `read_64`, then converts u64 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 8 != 0 {
            return Err(Error::Other(anyhow!(
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
    fn read_mem_32bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        // Default implementation uses `read_32`, then converts u32 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 4 != 0 {
            return Err(Error::Other(anyhow!(
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

    /// Read data from `address`.
    ///
    /// This function tries to use the fastest way of reading data, so there is no
    /// guarantee which kind of memory access is used. The function might also read more
    /// data than requested, e.g. when the start address is not aligned to a 32-bit boundary.
    ///
    /// For more control, the `read_x` functiongs, e.g. [`MemoryInterface::read_32()`], can be
    /// used.
    ///
    ///  Generally faster than `read_8`.
    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
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
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error>;

    /// Write a 32bit word at `address`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error>;

    /// Write an 8bit word at `address`.
    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error>;

    /// Write a block of 64bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error>;

    /// Write a block of 32bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error>;

    /// Write a block of 8bit words at `address`.
    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error>;

    /// Writes bytes using 64 bit memory access. Address must be 64 bit aligned
    /// and data must be an exact multiple of 8.
    fn write_mem_64bit(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        // Default implementation uses `write_64`, then converts u64 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 8 != 0 {
            return Err(Error::Other(anyhow!(
                "Call to read_mem_64bit with data.len() not a multiple of 8"
            )));
        }
        let mut buffer = vec![0u64; data.len() / 8];
        for (bytes, value) in data.chunks_exact(8).zip(buffer.iter_mut()) {
            *value = bytes
                .pread_with(0, scroll::LE)
                .expect("an u64 - this is a bug, please report it");
        }

        self.write_64(address, &buffer)?;
        Ok(())
    }

    /// Writes bytes using 32 bit memory access. Address must be 32 bit aligned
    /// and data must be an exact multiple of 8.
    fn write_mem_32bit(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        // Default implementation uses `write_32`, then converts u32 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 4 != 0 {
            return Err(Error::Other(anyhow!(
                "Call to read_mem_32bit with data.len() not a multiple of 4"
            )));
        }
        let mut buffer = vec![0u32; data.len() / 4];
        for (bytes, value) in data.chunks_exact(4).zip(buffer.iter_mut()) {
            *value = bytes
                .pread_with(0, scroll::LE)
                .expect("an u32 - this is a bug, please report it");
        }

        self.write_32(address, &buffer)?;
        Ok(())
    }

    /// Write a block of 8bit words at `address`. May use 64 bit memory access,
    /// so should only be used if reading memory locations that don't have side
    /// effects. Generally faster than [`MemoryInterface::write_8`].
    ///
    /// If the target does not support 8-bit aligned access, and `address` is not
    /// aligned on a 32-bit boundary, this function will return a [`Error::MemoryNotAligned`] error.
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        let len = data.len();
        let start_extra_count = 4 - (address % 4) as usize;
        let end_extra_count = (len - start_extra_count) % 4;
        let inbetween_count = len - start_extra_count - end_extra_count;
        assert!(start_extra_count < 4);
        assert!(end_extra_count < 4);
        assert!(inbetween_count % 4 == 0);

        // If we do not have 32 bit aligned access we first check that we can do 8 bit aligned access on this platform.
        // If we cannot we throw an error.
        // If we can we read the first n < 4 bytes up until the word aligned address that comes next.
        if address % 4 != 0 || len % 4 != 0 {
            // If we do not support 8 bit transfers we have to bail because we can only do 32 bit word aligned transers.
            if !self.supports_8bit_transfers()? {
                return Err(Error::MemoryNotAligned {
                    address,
                    alignment: 4,
                });
            }

            // We first do an 8 bit write of the first < 4 bytes up until the 4 byte aligned boundary.
            self.write_8(address, &data[..start_extra_count])?;
        }

        let mut buffer = vec![0u32; inbetween_count / 4];
        for (bytes, value) in data.chunks_exact(4).zip(buffer.iter_mut()) {
            *value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        self.write_32(address, &buffer)?;

        // We read the remaining bytes that we did not read yet which is always n < 4.
        if end_extra_count > 0 {
            self.write_8(address, &data[..start_extra_count])?;
        }

        Ok(())
    }

    /// Returns whether the current platform supports native 8bit transfers.
    fn supports_8bit_transfers(&self) -> Result<bool, Error>;

    /// Flush any outstanding operations.
    ///
    /// For performance, debug probe implementations may choose to batch writes;
    /// to assure that any such batched writes have in fact been issued, `flush`
    /// can be called.  Takes no arguments, but may return failure if a batched
    /// operation fails.
    fn flush(&mut self) -> Result<(), Error>;
}

impl<T> MemoryInterface for &mut T
where
    T: MemoryInterface,
{
    fn supports_native_64bit_access(&mut self) -> bool {
        (*self).supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        (*self).read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        (*self).read_word_32(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        (*self).read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        (*self).read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        (*self).read_32(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        (*self).read_8(address, data)
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        (*self).read(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        (*self).write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        (*self).write_word_32(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        (*self).write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        (*self).write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        (*self).write_32(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        (*self).write_8(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        (*self).write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        MemoryInterface::supports_8bit_transfers(*self)
    }

    fn flush(&mut self) -> Result<(), Error> {
        (*self).flush()
    }
}

// Helper functions to validate address space constraints

/// Validate that an input address is valid for 32-bit only systems
pub(crate) fn valid_32bit_address(address: u64) -> Result<u32, Error> {
    let address: u32 = address
        .try_into()
        .map_err(|_| anyhow!("Address {:#08x} out of range", address))?;

    Ok(address)
}
