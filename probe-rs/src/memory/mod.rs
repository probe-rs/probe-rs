use crate::error::Error;

use scroll::Pread;

/// {function_name} was called with data length that is not a multiple of {alignment}
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub struct InvalidDataLengthError {
    /// Name of the function that caused the error.
    pub function_name: &'static str,
    /// The alignment required on the data length.
    pub alignment: usize,
}
impl InvalidDataLengthError {
    pub fn new(function_name: &'static str, alignment: usize) -> Self {
        Self {
            function_name,
            alignment,
        }
    }
}

/// Memory access to address {address:#X?} was not aligned to {alignment} bytes.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub struct MemoryNotAlignedError {
    /// The address of the register.
    pub address: u64,
    /// The required alignment in bytes (address increments).
    pub alignment: usize,
}

/// An interface to be implemented for drivers that allow target memory access.
#[async_trait::async_trait(?Send)]
pub trait MemoryInterface<ERR = Error>
where
    ERR: std::error::Error + From<InvalidDataLengthError> + From<MemoryNotAlignedError>,
{
    /// Does this interface support native 64-bit wide accesses
    ///
    /// If false all 64-bit operations may be split into 32 or 8 bit operations.
    /// Most callers will not need to pivot on this but it can be useful for
    /// picking the fastest bulk data transfer method.
    async fn supports_native_64bit_access(&mut self) -> bool;

    /// Read a 64bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    async fn read_word_64(&mut self, address: u64) -> Result<u64, ERR> {
        let mut word = 0;
        self.read_64(address, std::slice::from_mut(&mut word))
            .await?;
        Ok(word)
    }

    /// Read a 32bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be a multiple of 4.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_word_32(&mut self, address: u64) -> Result<u32, ERR> {
        let mut word = 0;
        self.read_32(address, std::slice::from_mut(&mut word))
            .await?;
        Ok(word)
    }

    /// Read a 16bit word of at `address`.
    ///
    /// The address where the read should be performed at has to be a multiple of 2.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_word_16(&mut self, address: u64) -> Result<u16, ERR> {
        let mut word = 0;
        self.read_16(address, std::slice::from_mut(&mut word))
            .await?;
        Ok(word)
    }

    /// Read an 8bit word of at `address`.
    async fn read_word_8(&mut self, address: u64) -> Result<u8, ERR> {
        let mut word = 0;
        self.read_8(address, std::slice::from_mut(&mut word))
            .await?;
        Ok(word)
    }

    /// Read a block of 64bit words at `address` in the target's endianness.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ERR>;

    /// Read a block of 32bit words at `address` in the target's endianness.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 4.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ERR>;

    /// Read a block of 16bit words at `address` in the target's endianness.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 2.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ERR>;

    /// Read a block of 8bit words at `address`.
    async fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ERR>;

    /// Reads bytes using 64 bit memory access.
    ///
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_mem_64bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), ERR> {
        // Default implementation uses `read_64`, then converts u64 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 8 != 0 {
            return Err(InvalidDataLengthError::new("read_mem_64bit", 8).into());
        }
        let mut buffer = vec![0u64; data.len() / 8];
        self.read_64(address, &mut buffer).await?;
        for (bytes, value) in data.chunks_exact_mut(8).zip(buffer.iter()) {
            bytes.copy_from_slice(&u64::to_le_bytes(*value));
        }
        Ok(())
    }

    /// Reads bytes using 32 bit memory access.
    ///
    /// The address where the read should be performed at has to be a multiple of 4.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn read_mem_32bit(&mut self, address: u64, data: &mut [u8]) -> Result<(), ERR> {
        // Default implementation uses `read_32`, then converts u32 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 4 != 0 {
            return Err(InvalidDataLengthError::new("read_mem_32bit", 4).into());
        }
        let mut buffer = vec![0u32; data.len() / 4];
        self.read_32(address, &mut buffer).await?;
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
    /// For more control, the `read_x` functions, e.g. [`MemoryInterface::read_32()`], can be
    /// used.
    ///
    ///  Generally faster than `read_8`.
    async fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ERR> {
        if self.supports_native_64bit_access().await {
            // Avoid heap allocation and copy if we don't need it.
            self.read_8(address, data).await?;
        } else if address % 4 == 0 && data.len() % 4 == 0 {
            // Avoid heap allocation and copy if we don't need it.
            self.read_mem_32bit(address, data).await?;
        } else {
            let start_extra_count = (address % 4) as usize;
            let mut buffer = vec![0u8; (start_extra_count + data.len()).div_ceil(4) * 4];
            self.read_mem_32bit(address - start_extra_count as u64, &mut buffer)
                .await?;
            data.copy_from_slice(&buffer[start_extra_count..start_extra_count + data.len()]);
        }
        Ok(())
    }

    /// Write a 64bit word at `address`.
    ///
    /// The address where the write should be performed at has to be a multiple of 8.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), ERR> {
        self.write_64(address, std::slice::from_ref(&data)).await
    }

    /// Write a 32bit word at `address`.
    ///
    /// The address where the write should be performed at has to be a multiple of 4.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), ERR> {
        self.write_32(address, std::slice::from_ref(&data)).await
    }

    /// Write a 16bit word at `address`.
    ///
    /// The address where the write should be performed at has to be a multiple of 2.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), ERR> {
        self.write_16(address, std::slice::from_ref(&data)).await
    }

    /// Write an 8bit word at `address`.
    async fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), ERR> {
        self.write_8(address, std::slice::from_ref(&data)).await
    }

    /// Write a block of 64bit words at `address` in the target's endianness.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 8.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ERR>;

    /// Write a block of 32bit words at `address` in the target's endianness.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 4.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ERR>;

    /// Write a block of 16bit words at `address` in the target's endianness.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 2.
    /// Returns [`Error::MemoryNotAligned`] if this does not hold true.
    async fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ERR>;

    /// Write a block of 8bit words at `address`.
    async fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ERR>;

    /// Writes bytes using 64 bit memory access. Address must be 64 bit aligned
    /// and data must be an exact multiple of 8.
    async fn write_mem_64bit(&mut self, address: u64, data: &[u8]) -> Result<(), ERR> {
        // Default implementation uses `write_64`, then converts u64 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 8 != 0 {
            return Err(InvalidDataLengthError::new("write_mem_64bit", 8).into());
        }
        let mut buffer = vec![0u64; data.len() / 8];
        for (bytes, value) in data.chunks_exact(8).zip(buffer.iter_mut()) {
            *value = bytes
                .pread_with(0, scroll::LE)
                .expect("an u64 - this is a bug, please report it");
        }

        self.write_64(address, &buffer).await?;
        Ok(())
    }

    /// Writes bytes using 32 bit memory access. Address must be 32 bit aligned
    /// and data must be an exact multiple of 8.
    async fn write_mem_32bit(&mut self, address: u64, data: &[u8]) -> Result<(), ERR> {
        // Default implementation uses `write_32`, then converts u32 values back
        // to bytes. Assumes target is little endian. May be overridden to
        // provide an implementation that avoids heap allocation and endian
        // conversions. Must be overridden for big endian targets.
        if data.len() % 4 != 0 {
            return Err(InvalidDataLengthError::new("write_mem_32bit", 4).into());
        }
        let mut buffer = vec![0u32; data.len() / 4];
        for (bytes, value) in data.chunks_exact(4).zip(buffer.iter_mut()) {
            *value = bytes
                .pread_with(0, scroll::LE)
                .expect("an u32 - this is a bug, please report it");
        }

        self.write_32(address, &buffer).await?;
        Ok(())
    }

    /// Write a block of 8bit words at `address`. May use 64 bit memory access,
    /// so should only be used if reading memory locations that don't have side
    /// effects. Generally faster than [`MemoryInterface::write_8`].
    ///
    /// If the target does not support 8-bit aligned access, and `address` is not
    /// aligned on a 32-bit boundary, this function will return a [`Error::MemoryNotAligned`] error.
    async fn write(&mut self, mut address: u64, mut data: &[u8]) -> Result<(), ERR> {
        let len = data.len();
        let start_extra_count = ((4 - (address % 4) as usize) % 4).min(len);
        let end_extra_count = (len - start_extra_count) % 4;
        let inbetween_count = len - start_extra_count - end_extra_count;
        assert!(start_extra_count < 4);
        assert!(end_extra_count < 4);
        assert!(inbetween_count % 4 == 0);

        if start_extra_count != 0 || end_extra_count != 0 {
            // If we do not support 8 bit transfers we have to bail
            // because we have to do unaligned writes but can only do
            // 32 bit word aligned transers.
            if !self.supports_8bit_transfers().await? {
                return Err(MemoryNotAlignedError {
                    address,
                    alignment: 4,
                }
                .into());
            }
        }

        if start_extra_count != 0 {
            // We first do an 8 bit write of the first < 4 bytes up until the 4 byte aligned boundary.
            self.write_8(address, &data[..start_extra_count]).await?;

            address += start_extra_count as u64;
            data = &data[start_extra_count..];
        }

        // Make sure we don't try to do an empty but potentially unaligned write
        if inbetween_count > 0 {
            // We do a 32 bit write of the remaining bytes that are 4 byte aligned.
            let mut buffer = vec![0u32; inbetween_count / 4];
            for (bytes, value) in data.chunks_exact(4).zip(buffer.iter_mut()) {
                *value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            }
            self.write_32(address, &buffer).await?;

            address += inbetween_count as u64;
            data = &data[inbetween_count..];
        }

        // We write the remaining bytes that we did not write yet which is always n < 4.
        if end_extra_count > 0 {
            self.write_8(address, &data[..end_extra_count]).await?;
        }

        Ok(())
    }

    /// Returns whether the current platform supports native 8bit transfers.
    async fn supports_8bit_transfers(&self) -> Result<bool, ERR>;

    /// Flush any outstanding operations.
    ///
    /// For performance, debug probe implementations may choose to batch writes;
    /// to assure that any such batched writes have in fact been issued, `flush`
    /// can be called.  Takes no arguments, but may return failure if a batched
    /// operation fails.
    async fn flush(&mut self) -> Result<(), ERR>;
}

// Helper functions to validate address space constraints

/// Validate that an input address is valid for 32-bit only systems
pub(crate) fn valid_32bit_address(address: u64) -> Result<u32, Error> {
    let address: u32 = address
        .try_into()
        .map_err(|_| Error::Other(format!("Address {:#08x} out of range", address)))?;

    Ok(address)
}

/// Simplifies delegating MemoryInterface implementations, with additional error type conversion.
pub trait CoreMemoryInterface {
    type ErrorType: std::error::Error + From<InvalidDataLengthError> + From<MemoryNotAlignedError>;

    /// Returns a reference to the underlying memory interface.
    fn memory(&self) -> &dyn MemoryInterface<Self::ErrorType>;

    /// Returns a mutable reference to the underlying memory interface.
    fn memory_mut(&mut self) -> &mut dyn MemoryInterface<Self::ErrorType>;
}

#[async_trait::async_trait(?Send)]
impl<T> MemoryInterface<Error> for T
where
    T: CoreMemoryInterface,
    Error: From<<T as CoreMemoryInterface>::ErrorType>,
{
    async fn supports_native_64bit_access(&mut self) -> bool {
        self.memory_mut().supports_native_64bit_access().await
    }

    async fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.memory_mut()
            .read_word_64(address)
            .await
            .map_err(Error::from)
    }

    async fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.memory_mut()
            .read_word_32(address)
            .await
            .map_err(Error::from)
    }

    async fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.memory_mut()
            .read_word_16(address)
            .await
            .map_err(Error::from)
    }

    async fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.memory_mut()
            .read_word_8(address)
            .await
            .map_err(Error::from)
    }

    async fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.memory_mut()
            .read_64(address, data)
            .await
            .map_err(Error::from)
    }

    async fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.memory_mut()
            .read_32(address, data)
            .await
            .map_err(Error::from)
    }

    async fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.memory_mut()
            .read_16(address, data)
            .await
            .map_err(Error::from)
    }

    async fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.memory_mut()
            .read_8(address, data)
            .await
            .map_err(Error::from)
    }

    async fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.memory_mut().read(address, data).await.map_err(Error::from)
    }

    async fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.memory_mut()
            .write_word_64(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.memory_mut()
            .write_word_32(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.memory_mut()
            .write_word_16(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.memory_mut()
            .write_word_8(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.memory_mut()
            .write_64(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.memory_mut()
            .write_32(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.memory_mut()
            .write_16(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.memory_mut()
            .write_8(address, data)
            .await
            .map_err(Error::from)
    }

    async fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.memory_mut()
            .write(address, data)
            .await
            .map_err(Error::from)
    }

    async fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.memory()
            .supports_8bit_transfers()
            .await
            .map_err(Error::from)
    }

    async fn flush(&mut self) -> Result<(), Error> {
        self.memory_mut().flush().await.map_err(Error::from)
    }
}
