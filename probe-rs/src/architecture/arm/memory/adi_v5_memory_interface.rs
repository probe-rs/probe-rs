use super::super::ap::{
    AccessPortError, AddressIncrement, ApAccess, ApRegister, DataSize, MemoryAp, CSW, DRW, TAR,
    TAR2,
};
use crate::architecture::arm::communication_interface::{FlushableArmAccess, SwdSequence};
use crate::architecture::arm::{
    communication_interface::Initialized, dp::DpAccess, MemoryApInformation,
};
use crate::architecture::arm::{ArmCommunicationInterface, ArmError};
use crate::{probe::DebugProbeError, CoreStatus};

pub trait ArmProbe: SwdSequence {
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError>;

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError>;

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError>;

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError>;

    /// Reads a 64 bit word from `address`.
    fn read_word_64(&mut self, address: u64) -> Result<u64, ArmError> {
        let mut buff = [0];
        self.read_64(address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads a 32 bit word from `address`.
    fn read_word_32(&mut self, address: u64) -> Result<u32, ArmError> {
        let mut buff = [0];
        self.read_32(address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads a 16 bit word from `address`.
    fn read_word_16(&mut self, address: u64) -> Result<u16, ArmError> {
        let mut buff = [0];
        self.read_16(address, &mut buff)?;

        Ok(buff[0])
    }

    /// Reads an 8 bit word from `address`.
    fn read_word_8(&mut self, address: u64) -> Result<u8, ArmError> {
        let mut buff = [0];
        self.read_8(address, &mut buff)?;

        Ok(buff[0])
    }

    /// Read a block of 8bit words at `address`. May use 32 bit memory access,
    /// so should only be used if reading memory locations that don't have side
    /// effects. Generally faster than [`MemoryInterface::read_8`].
    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        let len = data.len();
        if address % 4 == 0 && len % 4 == 0 {
            let mut buffer = vec![0u32; len / 4];
            self.read_32(address, &mut buffer)?;
            for (bytes, value) in data.chunks_exact_mut(4).zip(buffer.iter()) {
                bytes.copy_from_slice(&u32::to_le_bytes(*value));
            }
        } else {
            let start_extra_count = (address % 4) as usize;
            let mut buffer = vec![0u32; (start_extra_count + len + 3) / 4];
            let read_address = address - start_extra_count as u64;
            self.read_32(read_address, &mut buffer)?;
            for (bytes, value) in data
                .chunks_exact_mut(4)
                .zip(buffer[start_extra_count..start_extra_count + len].iter())
            {
                bytes.copy_from_slice(&u32::to_le_bytes(*value));
            }
        }
        Ok(())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError>;

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError>;

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError>;

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError>;

    /// Writes a 64 bit word to `address`.
    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), ArmError> {
        self.write_64(address, &[data])
    }

    /// Writes a 32 bit word to `address`.
    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), ArmError> {
        self.write_32(address, &[data])
    }

    /// Writes a 16 bit word to `address`.
    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), ArmError> {
        self.write_16(address, &[data])
    }

    /// Writes a 8 bit word to `address`.
    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), ArmError> {
        self.write_8(address, &[data])
    }

    /// Write a block of 8bit words to `address`. May use 32 bit memory access,
    /// so it should only be used if writing memory locations that don't have side
    /// effects. Generally faster than [`MemoryInterface::write_8`].
    fn write(&mut self, mut address: u64, mut data: &[u8]) -> Result<(), ArmError> {
        let len = data.len();
        // Number of unaligned bytes at the start
        let start_extra_count = ((4 - (address % 4) as usize) % 4).min(len);
        // Extra bytes to be written at the end
        let end_extra_count = (len - start_extra_count) % 4;
        // Number of bytes between start and end (i.e. number of bytes transmitted as 32 bit words)
        let inbetween_count = len - start_extra_count - end_extra_count;

        assert!(start_extra_count < 4);
        assert!(end_extra_count < 4);
        assert!(inbetween_count % 4 == 0);

        // If we do not have 32 bit aligned access we first check that we can do 8 bit aligned access on this platform.
        // If we cannot we throw an error.
        // If we can we write the first n < 4 bytes up until the word aligned address that comes next.
        if address % 4 != 0 || len % 4 != 0 {
            // If we do not support 8 bit transfers we have to bail because we can only do 32 bit word aligned transers.
            if !self.supports_8bit_transfers()? {
                return Err(ArmError::alignment_error(address, 4));
            }

            // We first do an 8 bit write of the first < 4 bytes up until the 4 byte aligned boundary.
            self.write_8(address, &data[..start_extra_count])?;

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
            self.write_32(address, &buffer)?;

            address += inbetween_count as u64;
            data = &data[inbetween_count..];
        }

        // We write the remaining bytes that we did not write yet which is always n < 4.
        if end_extra_count > 0 {
            self.write_8(address, &data[..end_extra_count])?;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), ArmError>;

    fn supports_native_64bit_access(&mut self) -> bool;

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError>;

    /// Returns the underlying [`ApAddress`].
    fn ap(&mut self) -> MemoryAp;

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, state: CoreStatus) {
        self.get_arm_communication_interface()
            .map(|iface| iface.core_status_notification(state))
            .ok();
    }
}

/// Calculate the maximum number of bytes we can write starting at address
/// before we run into the 10-bit TAR autoincrement limit.
fn autoincr_max_bytes(address: u64) -> usize {
    const AUTOINCR_LIMIT: usize = 0x400;

    ((address + 1).next_multiple_of(AUTOINCR_LIMIT as _) - address) as usize
}

/// A struct to give access to a targets memory using a certain DAP.
pub(crate) struct ADIMemoryInterface<'interface, AP>
where
    AP: ApAccess + DpAccess,
{
    interface: &'interface mut AP,

    ap_information: MemoryApInformation,
    memory_ap: MemoryAp,

    /// Cached value of the CSW register, to avoid unnecessary writes.
    //
    /// TODO: This is the wrong location for this, it should actually be
    /// cached on a lower level, where the other Memory AP information is
    /// stored.
    cached_csw_value: Option<CSW>,
}

impl<'interface, AP> ADIMemoryInterface<'interface, AP>
where
    AP: ApAccess + DpAccess,
{
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(
        interface: &'interface mut AP,
        ap_information: MemoryApInformation,
    ) -> Result<ADIMemoryInterface<'interface, AP>, AccessPortError> {
        let address = ap_information.address.clone();
        Ok(Self {
            interface,
            ap_information,
            memory_ap: MemoryAp::new(address),
            cached_csw_value: None,
        })
    }
}

impl<AP> ADIMemoryInterface<'_, AP>
where
    AP: ApAccess + DpAccess,
{
    /// Build and write the correct CSW register for a memory access
    ///
    /// Currently, only AMBA AHB Access is supported.
    fn write_csw_register(&mut self, data_size: DataSize) -> Result<(), ArmError> {
        // The CSW Register is set for an AMBA AHB Acccess, according to
        // the ARM Debug Interface Architecture Specification.
        //
        // The HNONSEC bit is set according to [Self::supports_hnonsec]:
        // The PROT bits are set as follows:
        //  MasterType, bit [29] = 1  - Access as default AHB Master
        //  HPROT[4]             = 0  - Non-allocating access
        //
        // The CACHE bits are set for the following AHB access:
        //   HPROT[0] == 1   - data           access
        //   HPROT[1] == 1   - privileged     access
        //   HPROT[2] == 0   - non-bufferable access
        //   HPROT[3] == 1   - cacheable      access
        //
        // Setting cacheable indicates the request must not bypass the cache,
        // to ensure we observe the same state as the CPU core. On cores without
        // cache the bit is RAZ/WI.

        let value = CSW {
            DbgSwEnable: 0b1,
            HNONSEC: !self.ap_information.supports_hnonsec as u8,
            PROT: 0b10,
            CACHE: 0b1011,
            AddrInc: AddressIncrement::Single,
            SIZE: data_size,
            ..Default::default()
        };

        // Check if the write is necessary
        match self.cached_csw_value {
            Some(cached_value) if cached_value == value => Ok(()),
            _ => {
                self.write_ap_register(value)?;

                self.cached_csw_value = Some(value);

                Ok(())
            }
        }
    }

    fn write_tar_register(&mut self, address: u64) -> Result<(), ArmError> {
        let address_lower = address as u32;
        let address_upper = (address >> 32) as u32;

        let tar = TAR {
            address: address_lower,
        };
        self.write_ap_register(tar)?;

        if self.ap_information.has_large_address_extension {
            let tar = TAR2 {
                address: address_upper,
            };
            self.write_ap_register(tar)?;
        } else if address_upper != 0 {
            return Err(ArmError::OutOfBounds);
        }

        Ok(())
    }

    /// Read a 32 bit register on the given AP.
    fn read_ap_register<R>(&mut self) -> Result<R, ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .read_ap_register(&self.memory_ap)
            .map_err(AccessPortError::register_read_error::<R, _>)
            .map_err(|error| ArmError::from_access_port(error, &self.memory_ap))
    }

    /// Read multiple 32 bit values from the DRW register on the given AP.
    fn read_drw(&mut self, values: &mut [u32]) -> Result<(), ArmError>
    where
        AP: ApAccess,
    {
        if values.len() == 1 {
            // If transferring only 1 word, use non-repeated register access, because it might be faster depending on the probe.
            let drw: DRW = self.read_ap_register()?;
            values[0] = drw.data;
            Ok(())
        } else {
            self.interface
                .read_ap_register_repeated(&self.memory_ap, DRW { data: 0 }, values)
                .map_err(AccessPortError::register_read_error::<DRW, _>)
                .map_err(|err| ArmError::from_access_port(err, &self.memory_ap))
        }
    }

    /// Write a 32 bit register on the given AP.
    fn write_ap_register<R>(&mut self, register: R) -> Result<(), ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .write_ap_register(&self.memory_ap, register)
            .map_err(AccessPortError::register_write_error::<R, _>)
            .map_err(|e| ArmError::from_access_port(e, &self.memory_ap))
    }

    /// Write multiple 32 bit values to the DRW register on the given AP.
    fn write_drw(&mut self, values: &[u32]) -> Result<(), ArmError>
    where
        AP: ApAccess,
    {
        if values.len() == 1 {
            // If transferring only 1 word, use non-repeated register access, because it might be faster depending on the probe.
            self.write_ap_register(DRW { data: values[0] })
        } else {
            self.interface
                .write_ap_register_repeated(&self.memory_ap, DRW { data: 0 }, values)
                .map_err(AccessPortError::register_write_error::<DRW, _>)
                .map_err(|e| ArmError::from_access_port(e, &self.memory_ap))
        }
    }

    /// Read a block of 64 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_64(&mut self, mut address: u64, mut data: &mut [u64]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }

        if (address % 8) != 0 {
            return Err(ArmError::alignment_error(address, 8));
        }

        // Fall back to 32-bit accesses if 64-bit accesses are not supported.
        // In both cases the sequence of words we have to read from DRW is the same:
        // first the least significant word, then the most significant word.
        let size = match self.ap_information.has_large_data_extension {
            true => DataSize::U64,
            false => DataSize::U32,
        };
        self.write_csw_register(size)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 8);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;

            let mut buf = vec![0; chunk_size * 2];
            self.read_drw(&mut buf)?;

            for i in 0..chunk_size {
                data[i] = buf[i * 2] as u64 | (buf[i * 2 + 1] as u64) << 32;
            }

            address = address
                .checked_add(chunk_size as u64 * 8)
                .ok_or(ArmError::OutOfBounds)?;
            data = &mut data[chunk_size..];
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Read a block of 32 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 4.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_32(&mut self, mut address: u64, mut data: &mut [u32]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }

        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        self.write_csw_register(DataSize::U32)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 4);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.read_drw(&mut data[..chunk_size])?;

            address = address
                .checked_add(chunk_size as u64 * 4)
                .ok_or(ArmError::OutOfBounds)?;
            data = &mut data[chunk_size..];
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Read a block of 16 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 2.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_16(&mut self, mut address: u64, mut data: &mut [u16]) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if (address % 2) != 0 {
            return Err(ArmError::alignment_error(address, 2));
        }

        if data.is_empty() {
            return Ok(());
        }

        self.write_csw_register(DataSize::U16)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 2);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            let mut values = vec![0; chunk_size];

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.read_drw(&mut values)?;

            // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
            // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
            // we have to shift the word (one or two bytes) to it's correct position.
            for (target, (i, source)) in
                data[..chunk_size].iter_mut().zip(values.iter().enumerate())
            {
                *target = ((*source >> (((address + i as u64 * 2) % 4) * 8)) & 0xFFFF) as u16;
            }

            address = address
                .checked_add(chunk_size as u64 * 2)
                .ok_or(ArmError::OutOfBounds)?;
            data = &mut data[chunk_size..];
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Read a block of 8 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    pub fn read_8(&mut self, mut address: u64, mut data: &mut [u8]) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if data.is_empty() {
            return Ok(());
        }

        self.write_csw_register(DataSize::U8)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address));

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            let mut values = vec![0; chunk_size];

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.read_drw(&mut values)?;

            // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
            // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
            // we have to shift the word (one or two bytes) to it's correct position.
            for (target, (i, source)) in
                data[..chunk_size].iter_mut().zip(values.iter().enumerate())
            {
                *target = ((*source >> (((address + i as u64) % 4) * 8)) & 0xFF) as u8;
            }

            address = address
                .checked_add(chunk_size as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data = &mut data[chunk_size..];
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Write a block of 64 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_64(&mut self, mut address: u64, mut data: &[u64]) -> Result<(), ArmError> {
        if (address % 8) != 0 {
            return Err(ArmError::alignment_error(address, 8));
        }

        if data.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len() * 8,
            address
        );

        // Fall back to 32-bit accesses if 64-bit accesses are not supported.
        // In both cases the sequence of words we have to write to DRW is the same:
        // first the least significant word, then the most significant word.
        let size = match self.ap_information.has_large_data_extension {
            true => DataSize::U64,
            false => DataSize::U32,
        };
        self.write_csw_register(size)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 8);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            let values: Vec<u32> = data[..chunk_size]
                .iter()
                .flat_map(|&w| [w as u32, (w >> 32) as u32])
                .collect();

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.write_drw(&values)?;

            address = address
                .checked_add(chunk_size as u64 * 8)
                .ok_or(ArmError::OutOfBounds)?;
            data = &data[chunk_size..];
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 32 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 4.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_32(&mut self, mut address: u64, mut data: &[u32]) -> Result<(), ArmError> {
        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        if data.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len() * 4,
            address
        );

        self.write_csw_register(DataSize::U32)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 4);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.write_drw(&data[..chunk_size])?;

            address = address
                .checked_add(chunk_size as u64 * 4)
                .ok_or(ArmError::OutOfBounds)?;
            data = &data[chunk_size..];
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 16 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 2.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_16(&mut self, mut address: u64, mut data: &[u16]) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }
        if (address % 2) != 0 {
            return Err(ArmError::alignment_error(address, 2));
        }
        if data.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len() * 2,
            address
        );

        self.write_csw_register(DataSize::U16)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 2);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
            // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
            // we have to shift the word (one or two bytes) to it's correct position.
            let values = data[..chunk_size]
                .iter()
                .enumerate()
                .map(|(i, v)| (*v as u32) << (((address as usize + i * 2) % 4) * 8))
                .collect::<Vec<_>>();

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.write_drw(&values)?;

            address = address
                .checked_add(chunk_size as u64 * 2)
                .ok_or(ArmError::OutOfBounds)?;
            data = &data[chunk_size..];
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 8 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    pub fn write_8(&mut self, mut address: u64, mut data: &[u8]) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if data.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len(),
            address
        );

        self.write_csw_register(DataSize::U8)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address));

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
            // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
            // we have to shift the word (one or two bytes) to it's correct position.
            let values = data[..chunk_size]
                .iter()
                .enumerate()
                .map(|(i, v)| (*v as u32) << (((address as usize + i) % 4) * 8))
                .collect::<Vec<_>>();

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.write_tar_register(address)?;
            self.write_drw(&values)?;

            address = address
                .checked_add(chunk_size as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data = &data[chunk_size..];
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }
}

impl<AP> SwdSequence for ADIMemoryInterface<'_, AP>
where
    AP: FlushableArmAccess + ApAccess + DpAccess,
{
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.get_arm_communication_interface()?
            .swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.get_arm_communication_interface()?
            .swj_pins(pin_out, pin_select, pin_wait)
    }
}

impl<AP> ArmProbe for ADIMemoryInterface<'_, AP>
where
    AP: FlushableArmAccess + ApAccess + DpAccess,
{
    fn supports_native_64bit_access(&mut self) -> bool {
        self.ap_information.has_large_data_extension
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        self.read_8(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        self.read_16(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        self.read_32(address, data)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        self.read_64(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        self.write_8(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        self.write_16(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        self.write_32(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        self.write_64(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(!self.ap_information.supports_only_32bit_data_size)
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        self.interface.flush()?;

        Ok(())
    }

    /// Returns the underlying [`ApAddress`].
    fn ap(&mut self) -> MemoryAp {
        self.memory_ap.clone()
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError> {
        FlushableArmAccess::get_arm_communication_interface(self.interface)
    }
}

#[cfg(test)]
mod tests {
    use scroll::Pread;

    use crate::architecture::arm::{ap::AccessPort, FullyQualifiedApAddress, MemoryApInformation};

    use super::super::super::ap::memory_ap::mock::MockMemoryAp;
    use super::super::super::ap::memory_ap::MemoryAp;
    use super::ADIMemoryInterface;
    use super::ArmProbe;

    const DUMMY_AP: MemoryAp = MemoryAp::new(FullyQualifiedApAddress::v1_with_default_dp(0));

    impl<'interface> ADIMemoryInterface<'interface, MockMemoryAp> {
        /// Creates a new MemoryInterface for given AccessPort.
        fn new_mock(
            mock: &'interface mut MockMemoryAp,
        ) -> ADIMemoryInterface<'interface, MockMemoryAp> {
            let ap_information = MemoryApInformation {
                address: DUMMY_AP.ap_address().clone(),
                supports_only_32bit_data_size: false,
                supports_hnonsec: false,
                debug_base_address: 0xf000_0000,
                has_large_address_extension: false,
                has_large_data_extension: false,
                device_enabled: true,
            };

            Self::new(mock, ap_information).unwrap()
        }

        fn mock_memory(&self) -> &[u8] {
            &self.interface.memory
        }
    }

    // Visually obvious pattern used to test memory writes
    const DATA8: &[u8] = &[
        128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
    ];

    // DATA8 interpreted as little endian 16-bit words
    const DATA16: &[u16] = &[
        0x8180, 0x8382, 0x8584, 0x8786, 0x8988, 0x8b8a, 0x8d8c, 0x8f8e,
    ];

    // DATA8 interpreted as little endian 32-bit words
    const DATA32: &[u32] = &[0x83828180, 0x87868584, 0x8b8a8988, 0x8f8e8d8c];

    #[test]
    fn read_word_32() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 4] {
            let value = mi.read_word_32(address).expect("read_word_32 failed");
            assert_eq!(value, DATA32[address as usize / 4]);
        }
    }

    #[test]
    fn read_word_16() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 2, 4, 6] {
            let value = mi.read_word_16(address).expect("read_word_16 failed");
            assert_eq!(value, DATA16[address as usize / 2]);
        }
    }

    #[test]
    fn read_word_8() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..8 {
            let value = mi
                .read_word_8(address)
                .unwrap_or_else(|_| panic!("read_word_8 failed, address = {address}"));
            assert_eq!(value, DATA8[address as usize], "address = {address}");
        }
    }

    #[test]
    fn write_word_32() {
        for address in [0, 4] {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address as usize..][..4].copy_from_slice(&DATA8[..4]);

            mi.write_word_32(address, DATA32[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn write_word_16() {
        for address in [0, 2, 4, 6] {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address as usize..][..2].copy_from_slice(&DATA8[..2]);

            mi.write_word_16(address, DATA16[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn write_word_8() {
        for address in 0..8 {
            let mut mock = MockMemoryAp::with_pattern_and_size(256);
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address] = DATA8[0];

            mi.write_word_8(address as u64, DATA8[0])
                .unwrap_or_else(|_| panic!("write_word_8 failed, address = {address}"));
            assert_eq!(mi.mock_memory(), expected.as_slice(), "address = {address}");
        }
    }

    #[test]
    fn read_32() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 4] {
            for len in 0..3 {
                let mut data = vec![0u32; len];
                mi.read_32(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_32 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA32[(address / 4) as usize..(address / 4) as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn read_32_big_chunk() {
        let mut mock = MockMemoryAp::with_pattern_and_size(4096);
        let expected: Vec<u32> = mock
            .memory
            .chunks(4)
            .map(|b| b.pread(0).unwrap())
            .take(513)
            .collect();
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        let mut data = vec![0u32; 513];
        mi.read_32(0, &mut data)
            .unwrap_or_else(|_| panic!("read_32 failed, address = {}, len = {}", 0, data.len()));

        assert_eq!(
            data.as_slice(),
            expected,
            "address = {}, len = {}",
            0,
            data.len()
        );
    }

    #[test]
    fn read_32_unaligned_should_error() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [1, 3, 127] {
            assert!(mi.read_32(address, &mut [0u32; 4]).is_err());
        }
    }

    #[test]
    fn read_16() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [0, 2, 4, 6] {
            for len in 0..4 {
                let mut data = vec![0u16; len];
                mi.read_16(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_16 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA16[(address / 2) as usize..(address / 2) as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test_log::test]
    fn read_8() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                mi.read_8(address, &mut data)
                    .unwrap_or_else(|_| panic!("read_8 failed, address = {address}, len = {len}"));

                assert_eq!(
                    data.as_slice(),
                    &DATA8[address as usize..address as usize + len],
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_32() {
        for address in [0, 4] {
            for len in 0..3 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len * 4]
                    .copy_from_slice(&DATA8[..len * 4]);

                let data = &DATA32[..len];
                mi.write_32(address, data).unwrap_or_else(|_| {
                    panic!("write_32 failed, address = {address}, len = {len}")
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_16() {
        for address in [0, 2, 4, 6] {
            for len in 0..3 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len * 2]
                    .copy_from_slice(&DATA8[..len * 2]);

                let data = &DATA16[..len];
                mi.write_16(address, data).unwrap_or_else(|_| {
                    panic!("write_16 failed, address = {address}, len = {len}")
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in [1, 3, 127] {
            assert!(mi.write_32(address, &[0xDEAD_BEEF, 0xABBA_BABE]).is_err());
        }
    }

    #[test]
    fn write_8() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                mi.write_8(address, data)
                    .unwrap_or_else(|_| panic!("write_8 failed, address = {address}, len = {len}"));

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }

    #[test]
    fn write() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAp::with_pattern_and_size(256);
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                mi.write(address, data)
                    .unwrap_or_else(|_| panic!("write failed, address = {address}, len = {len}"));

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {address}, len = {len}"
                );
            }
        }
    }
}
