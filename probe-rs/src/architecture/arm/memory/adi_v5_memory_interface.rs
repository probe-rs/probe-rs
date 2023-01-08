use super::super::ap::{
    AccessPortError, AddressIncrement, ApAccess, ApRegister, DataSize, MemoryAp, CSW, DRW, TAR,
    TAR2,
};
use crate::architecture::arm::communication_interface::{FlushableArmAccess, SwdSequence};
use crate::architecture::arm::{
    communication_interface::Initialized, dp::DpAccess, MemoryApInformation,
};
use crate::architecture::arm::{ArmCommunicationInterface, ArmError};
use crate::DebugProbeError;
use std::convert::TryInto;
use std::ops::Range;

pub trait ArmProbe: SwdSequence {
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError>;

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

    /// Writes a 8 bit word to `address`.
    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), ArmError> {
        self.write_8(address, &[data])
    }

    /// Write a block of 8bit words to `address`. May use 32 bit memory access,
    /// so it should only be used if writing memory locations that don't have side
    /// effects. Generally faster than [`MemoryInterface::write_8`].
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
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
                return Err(ArmError::alignment_error(address, 4));
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

    fn flush(&mut self) -> Result<(), ArmError>;

    fn supports_native_64bit_access(&mut self) -> bool;

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError>;

    /// Returns the underlying [`ApAddress`].
    fn ap(&mut self) -> MemoryAp;

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;
}

/// A struct to give access to a targets memory using a certain DAP.
pub(crate) struct ADIMemoryInterface<'interface, AP>
where
    AP: ApAccess + DpAccess,
{
    interface: &'interface mut AP,

    ap_information: MemoryApInformation,
    memory_ap: MemoryAp,

    /// Cached value of the CSW register, to avoid unecessary writes.
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
        let address = ap_information.address;
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
    /// Build the correct CSW register for a memory access
    ///
    /// Currently, only AMBA AHB Access is supported.
    fn build_csw_register(&self, data_size: DataSize) -> CSW {
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
        //   HPROT[2] == 0   - non-cacheable  access
        //   HPROT[3] == 0   - non-bufferable access

        CSW {
            HNONSEC: !self.ap_information.supports_hnonsec as u8,
            PROT: 0b10,
            CACHE: 0b11,
            AddrInc: AddressIncrement::Single,
            SIZE: data_size,
            ..Default::default()
        }
    }

    fn write_csw_register(&mut self, access_port: MemoryAp, value: CSW) -> Result<(), ArmError> {
        // Check if the write is necessary
        match self.cached_csw_value {
            Some(cached_value) if cached_value == value => Ok(()),
            _ => {
                self.write_ap_register(access_port, value)?;

                self.cached_csw_value = Some(value);

                Ok(())
            }
        }
    }

    fn write_tar_register(&mut self, access_port: MemoryAp, address: u64) -> Result<(), ArmError> {
        let address_lower = address as u32;
        let address_upper = (address >> 32) as u32;

        let tar = TAR {
            address: address_lower,
        };
        self.write_ap_register(access_port, tar)?;

        if self.ap_information.has_large_address_extension {
            let tar = TAR2 {
                address: address_upper,
            };
            self.write_ap_register(access_port, tar)?;
        } else if address_upper != 0 {
            return Err(ArmError::OutOfBounds);
        }

        Ok(())
    }

    /// Read a 32 bit register on the given AP.
    fn read_ap_register<R>(&mut self, access_port: MemoryAp) -> Result<R, ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .read_ap_register(access_port)
            .map_err(AccessPortError::register_read_error::<R, _>)
            .map_err(|error| ArmError::from_access_port(error, access_port))
    }

    /// Read multiple 32 bit values from the same
    /// register on the given AP.
    fn read_ap_register_repeated<R>(
        &mut self,
        access_port: MemoryAp,
        register: R,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .read_ap_register_repeated(access_port, register, values)
            .map_err(AccessPortError::register_read_error::<R, _>)
            .map_err(|err| ArmError::from_access_port(err, access_port))
    }

    /// Write a 32 bit register on the given AP.
    fn write_ap_register<R>(&mut self, access_port: MemoryAp, register: R) -> Result<(), ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .write_ap_register(access_port, register)
            .map_err(AccessPortError::register_write_error::<R, _>)
            .map_err(|e| ArmError::from_access_port(e, access_port))
    }

    /// Write multiple 32 bit values to the same
    /// register on the given AP.
    fn write_ap_register_repeated<R>(
        &mut self,
        access_port: MemoryAp,
        register: R,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .write_ap_register_repeated(access_port, register, values)
            .map_err(AccessPortError::register_write_error::<R, _>)
            .map_err(|e| ArmError::from_access_port(e, access_port))
    }

    /// Read a 64bit word at `address`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_word_64(&mut self, access_port: MemoryAp, address: u64) -> Result<u64, ArmError> {
        if (address % 8) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        if !self.ap_information.has_large_data_extension {
            let mut ret: u64 = self.read_word_32(access_port, address)? as u64;
            ret |= (self.read_word_32(access_port, address + 4)? as u64) << 32;

            Ok(ret)
        } else {
            let csw = self.build_csw_register(DataSize::U64);

            self.write_csw_register(access_port, csw)?;
            self.write_tar_register(access_port, address)?;

            let result: DRW = self.read_ap_register(access_port)?;

            let mut ret = result.data as u64;
            let result: DRW = self.read_ap_register(access_port)?;
            ret |= (result.data as u64) << 32;

            Ok(ret)
        }
    }

    /// Read a 32bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_word_32(&mut self, access_port: MemoryAp, address: u64) -> Result<u32, ArmError> {
        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        let csw = self.build_csw_register(DataSize::U32);

        self.write_csw_register(access_port, csw)?;
        self.write_tar_register(access_port, address)?;
        let result: DRW = self.read_ap_register(access_port)?;

        Ok(result.data)
    }

    /// Read an 8 bit word at `address`.
    pub fn read_word_8(&mut self, access_port: MemoryAp, address: u64) -> Result<u8, ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        let aligned = aligned_range(address, 1)?;

        // Offset of byte in word (little endian)
        let bit_offset = (address - aligned.start) * 8;

        let csw = self.build_csw_register(DataSize::U8);
        self.write_csw_register(access_port, csw)?;
        self.write_tar_register(access_port, address)?;
        let result: DRW = self.read_ap_register(access_port)?;

        // Extract the correct byte
        // See "Arm Debug Interface Architecture Specification ADIv5.0 to ADIv5.2", C2.2.6
        Ok(((result.data >> bit_offset) & 0xFF) as u8)
    }

    /// Read a block of 32 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn read_32(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: &mut [u32],
    ) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }

        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        // Second we read in 32 bit reads until we have less than 32 bits left to read.
        let csw = self.build_csw_register(DataSize::U32);
        self.write_csw_register(access_port, csw)?;
        self.write_tar_register(access_port, address)?;

        // The maximum chunk size we can read before data overflows.
        // This is the size of the internal counter that is used for the address increment in the ARM spec.
        let max_chunk_size_bytes = 0x400;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len() * 4,
        );

        let mut data_offset = 0;

        tracing::debug!(
            "Read first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_transfer_unit = first_chunk_size_bytes / 4;

        self.read_ap_register_repeated(
            access_port,
            DRW { data: 0 },
            &mut data[data_offset..first_chunk_size_transfer_unit],
        )?;

        remaining_data_len -= first_chunk_size_transfer_unit;
        let mut address = address
            .checked_add((4 * first_chunk_size_transfer_unit) as u64)
            .ok_or(ArmError::OutOfBounds)?;
        data_offset += first_chunk_size_transfer_unit;

        while remaining_data_len > 0 {
            // the autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows
            self.write_tar_register(access_port, address)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_transfer_unit = next_chunk_size_bytes / 4;

            self.read_ap_register_repeated(
                access_port,
                DRW { data: 0 },
                &mut data[data_offset..(data_offset + next_chunk_size_transfer_unit)],
            )?;

            remaining_data_len -= next_chunk_size_transfer_unit;
            address = address
                .checked_add((4 * next_chunk_size_transfer_unit) as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data_offset += next_chunk_size_transfer_unit;
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Read a block of 8 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    pub fn read_8(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: &mut [u8],
    ) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if data.is_empty() {
            return Ok(());
        }

        let start_address = address;
        let mut data_u32 = vec![0u32; data.len()];

        let csw = self.build_csw_register(DataSize::U8);
        self.write_csw_register(access_port, csw)?;

        let mut address = address;
        self.write_tar_register(access_port, address)?;

        // The maximum chunk size we can read before data overflows.
        // This is the size of the internal counter that is used for the address increment in the ARM spec.
        let max_chunk_size_bytes = 0x400;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len(),
        );

        let mut data_offset = 0;

        tracing::debug!(
            "Read first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_transfer_unit = first_chunk_size_bytes;

        self.read_ap_register_repeated(
            access_port,
            DRW { data: 0 },
            &mut data_u32[data_offset..first_chunk_size_transfer_unit],
        )?;

        remaining_data_len -= first_chunk_size_transfer_unit;
        address = address
            .checked_add((first_chunk_size_transfer_unit) as u64)
            .ok_or(ArmError::OutOfBounds)?;
        data_offset += first_chunk_size_transfer_unit;

        while remaining_data_len > 0 {
            // The autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows.
            self.write_tar_register(access_port, address)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_transfer_unit = next_chunk_size_bytes;

            self.read_ap_register_repeated(
                access_port,
                DRW { data: 0 },
                &mut data_u32[data_offset..(data_offset + next_chunk_size_transfer_unit)],
            )?;

            remaining_data_len -= next_chunk_size_transfer_unit;
            address = address
                .checked_add((next_chunk_size_transfer_unit) as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data_offset += next_chunk_size_transfer_unit;
        }

        // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
        // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
        // we have to shift the word (one or two bytes) to it's correct position.
        for (target, (i, source)) in data.iter_mut().zip(data_u32.iter().enumerate()) {
            *target = ((*source >> (((start_address + i as u64) % 4) * 8)) & 0xFF) as u8;
        }

        tracing::debug!("Finished reading block");

        Ok(())
    }

    /// Write a 64bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_word_64(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: u64,
    ) -> Result<(), ArmError> {
        if (address % 8) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        let low_word = data as u32;
        let high_word = (data >> 32) as u32;

        if !self.ap_information.has_large_data_extension {
            self.write_word_32(access_port, address, low_word)?;
            self.write_word_32(access_port, address + 4, high_word)
        } else {
            let csw = self.build_csw_register(DataSize::U64);
            let drw = DRW { data: low_word };

            self.write_csw_register(access_port, csw)?;

            self.write_tar_register(access_port, address)?;
            self.write_ap_register(access_port, drw)?;

            let drw = DRW { data: high_word };
            self.write_ap_register(access_port, drw)?;

            Ok(())
        }
    }

    /// Write a 32bit word at `address`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_word_32(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: u32,
    ) -> Result<(), ArmError> {
        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };

        self.write_csw_register(access_port, csw)?;

        self.write_tar_register(access_port, address)?;
        self.write_ap_register(access_port, drw)?;

        Ok(())
    }

    /// Write an 8 bit word at `address`.
    pub fn write_word_8(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: u8,
    ) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        let aligned = aligned_range(address, 1)?;

        // Offset of byte in word (little endian)
        let bit_offset = (address - aligned.start) * 8;

        let csw = self.build_csw_register(DataSize::U8);
        let drw = DRW {
            data: u32::from(data) << bit_offset,
        };
        self.write_csw_register(access_port, csw)?;
        self.write_tar_register(access_port, address)?;
        self.write_ap_register(access_port, drw)?;

        Ok(())
    }

    /// Write a block of 32 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    pub fn write_32(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: &[u32],
    ) -> Result<(), ArmError> {
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

        // Second we write in 32 bit reads until we have less than 32 bits left to write.
        let csw = self.build_csw_register(DataSize::U32);

        self.write_csw_register(access_port, csw)?;

        self.write_tar_register(access_port, address)?;

        // maximum chunk size
        let max_chunk_size_bytes = 0x400_usize;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len() * 4,
        );

        let mut data_offset = 0;

        tracing::debug!(
            "Write first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_transfer_unit = first_chunk_size_bytes / 4;

        self.write_ap_register_repeated(
            access_port,
            DRW { data: 0 },
            &data[data_offset..first_chunk_size_transfer_unit],
        )?;

        remaining_data_len -= first_chunk_size_transfer_unit;
        let mut address = address
            .checked_add((first_chunk_size_transfer_unit * 4) as u64)
            .ok_or(ArmError::OutOfBounds)?;
        data_offset += first_chunk_size_transfer_unit;

        while remaining_data_len > 0 {
            // the autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows
            self.write_tar_register(access_port, address)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_transfer_unit = next_chunk_size_bytes / 4;

            self.write_ap_register_repeated(
                access_port,
                DRW { data: 0 },
                &data[data_offset..(data_offset + next_chunk_size_transfer_unit)],
            )?;

            remaining_data_len -= next_chunk_size_transfer_unit;
            address = address
                .checked_add((next_chunk_size_transfer_unit * 4) as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data_offset += next_chunk_size_transfer_unit;
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 8 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    pub fn write_8(
        &mut self,
        access_port: MemoryAp,
        address: u64,
        data: &[u8],
    ) -> Result<(), ArmError> {
        if self.ap_information.supports_only_32bit_data_size {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if data.is_empty() {
            return Ok(());
        }

        // The required shifting logic here is described in C2.2.6 Byte lanes of the ADI v5.2 specification.
        // All bytes are transfered in their lane, so when we do an access at an address that is not divisible by 4,
        // we have to shift the word (one or two bytes) to it's correct position.
        let data = data
            .iter()
            .enumerate()
            .map(|(i, v)| (*v as u32) << (((address as usize + i) % 4) * 8))
            .collect::<Vec<_>>();

        tracing::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len(),
            address
        );

        // Second we write in 8 bit writes until we have less than 8 bits left to write.
        let csw = self.build_csw_register(DataSize::U8);

        self.write_csw_register(access_port, csw)?;
        self.write_tar_register(access_port, address)?;

        // figure out how many words we can write before the
        // data overflows

        // maximum chunk size
        let max_chunk_size_bytes = 0x400_usize;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len(),
        );

        let mut data_offset = 0;

        tracing::debug!(
            "Write first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_transfer_unit = first_chunk_size_bytes;

        self.write_ap_register_repeated(
            access_port,
            DRW { data: 0 },
            &data[data_offset..first_chunk_size_transfer_unit],
        )?;

        remaining_data_len -= first_chunk_size_transfer_unit;
        let mut address = address
            .checked_add((first_chunk_size_transfer_unit) as u64)
            .ok_or(ArmError::OutOfBounds)?;
        data_offset += first_chunk_size_transfer_unit;

        while remaining_data_len > 0 {
            // the autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows
            self.write_tar_register(access_port, address)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_transfer_unit = next_chunk_size_bytes;

            self.write_ap_register_repeated(
                access_port,
                DRW { data: 0 },
                &data[data_offset..(data_offset + next_chunk_size_transfer_unit)],
            )?;

            remaining_data_len -= next_chunk_size_transfer_unit;
            address = address
                .checked_add((next_chunk_size_transfer_unit) as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data_offset += next_chunk_size_transfer_unit;
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
        if data.len() == 1 {
            data[0] = self.read_word_8(self.memory_ap, address)?;
        } else {
            self.read_8(self.memory_ap, address, data)?;
        }

        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        if data.len() == 1 {
            data[0] = self.read_word_32(self.memory_ap, address)?;
        } else {
            self.read_32(self.memory_ap, address, data)?;
        }

        Ok(())
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        for (i, d) in data.iter_mut().enumerate() {
            *d = self.read_word_64(self.memory_ap, address + (i as u64 * 8))?;
        }

        Ok(())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        if data.len() == 1 {
            self.write_word_8(self.memory_ap, address, data[0])?;
        } else {
            self.write_8(self.memory_ap, address, data)?;
        }

        Ok(())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        if data.len() == 1 {
            self.write_word_32(self.memory_ap, address, data[0])?;
        } else {
            self.write_32(self.memory_ap, address, data)?;
        }

        Ok(())
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        for (i, d) in data.iter().enumerate() {
            self.write_word_64(self.memory_ap, address + (i as u64 * 8), *d)?;
        }

        Ok(())
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
        self.memory_ap
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError> {
        FlushableArmAccess::get_arm_communication_interface(self.interface)
    }
}

/// Calculates a 32-bit word aligned range from an address/length pair.
fn aligned_range(address: u64, len: usize) -> Result<Range<u64>, ArmError> {
    // Round start address down to the nearest multiple of 4
    let start = address - (address % 4);

    let unaligned_end = len
        .try_into()
        .ok()
        .and_then(|len: u64| len.checked_add(address))
        .ok_or(ArmError::OutOfBounds)?;

    // Round end address up to the nearest multiple of 4
    let end = unaligned_end
        .checked_add((4 - (unaligned_end % 4)) % 4)
        .ok_or(ArmError::OutOfBounds)?;

    Ok(Range { start, end })
}

#[cfg(test)]
mod tests {
    use scroll::Pread;

    use crate::architecture::arm::{ap::AccessPort, ApAddress, DpAddress, MemoryApInformation};

    use super::super::super::ap::memory_ap::mock::MockMemoryAp;
    use super::super::super::ap::memory_ap::MemoryAp;
    use super::ADIMemoryInterface;

    const DUMMY_AP: MemoryAp = MemoryAp::new(ApAddress {
        dp: DpAddress::Default,
        ap: 0,
    });

    impl<'interface> ADIMemoryInterface<'interface, MockMemoryAp> {
        /// Creates a new MemoryInterface for given AccessPort.
        fn new_mock(
            mock: &'interface mut MockMemoryAp,
        ) -> ADIMemoryInterface<'interface, MockMemoryAp> {
            let ap_information = MemoryApInformation {
                address: DUMMY_AP.ap_address(),
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

    // DATA8 interpreted as little endian 32-bit words
    const DATA32: &[u32] = &[0x83828180, 0x87868584, 0x8b8a8988, 0x8f8e8d8c];

    #[test]
    fn read_word_32() {
        let mut mock = MockMemoryAp::with_pattern();
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for &address in &[0, 4] {
            let value = mi
                .read_word_32(DUMMY_AP, address)
                .expect("read_word_32 failed");
            assert_eq!(value, DATA32[address as usize / 4]);
        }
    }

    #[test]
    fn read_word_8() {
        let mut mock = MockMemoryAp::with_pattern();
        mock.memory[..8].copy_from_slice(&DATA8[..8]);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..8 {
            let value = mi
                .read_word_8(DUMMY_AP, address)
                .unwrap_or_else(|_| panic!("read_word_8 failed, address = {}", address));
            assert_eq!(value, DATA8[address as usize], "address = {}", address);
        }
    }

    #[test]
    fn write_word_32() {
        for &address in &[0, 4] {
            let mut mock = MockMemoryAp::with_pattern();
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[(address as usize)..(address as usize) + 4].copy_from_slice(&DATA8[..4]);

            mi.write_word_32(DUMMY_AP, address, DATA32[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {}", address));
            assert_eq!(
                mi.mock_memory(),
                expected.as_slice(),
                "address = {}",
                address
            );
        }
    }

    #[test]
    fn write_word_8() {
        for address in 0..8 {
            let mut mock = MockMemoryAp::with_pattern();
            let mut mi = ADIMemoryInterface::new_mock(&mut mock);

            let mut expected = Vec::from(mi.mock_memory());
            expected[address] = DATA8[0];

            mi.write_word_8(DUMMY_AP, address as u64, DATA8[0])
                .unwrap_or_else(|_| panic!("write_word_8 failed, address = {}", address));
            assert_eq!(
                mi.mock_memory(),
                expected.as_slice(),
                "address = {}",
                address
            );
        }
    }

    #[test]
    fn read_32() {
        let mut mock = MockMemoryAp::with_pattern();
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for &address in &[0, 4] {
            for len in 0..3 {
                let mut data = vec![0u32; len];
                mi.read_32(DUMMY_AP, address, &mut data)
                    .unwrap_or_else(|_| {
                        panic!("read_32 failed, address = {}, len = {}", address, len)
                    });

                assert_eq!(
                    data.as_slice(),
                    &DATA32[(address / 4) as usize..(address / 4) as usize + len],
                    "address = {}, len = {}",
                    address,
                    len
                );
            }
        }
    }

    #[test]
    fn read_32_big_chunk() {
        let mut mock = MockMemoryAp::with_pattern();
        let expected: Vec<u32> = mock
            .memory
            .chunks(4)
            .map(|b| b.pread(0).unwrap())
            .take(513)
            .collect();
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        let mut data = vec![0u32; 513];
        mi.read_32(DUMMY_AP, 0, &mut data)
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
        let mut mock = MockMemoryAp::with_pattern();
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for &address in &[1, 3, 127] {
            assert!(mi.read_32(DUMMY_AP, address, &mut [0u32; 4]).is_err());
        }
    }

    #[test]
    fn read_8() {
        let mut mock = MockMemoryAp::with_pattern();
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                mi.read_8(DUMMY_AP, address, &mut data).unwrap_or_else(|_| {
                    panic!("read_8 failed, address = {}, len = {}", address, len)
                });

                assert_eq!(
                    data.as_slice(),
                    &DATA8[address as usize..address as usize + len],
                    "address = {}, len = {}",
                    address,
                    len
                );
            }
        }
    }

    #[test]
    fn write_32() {
        for &address in &[0, 4] {
            for len in 0..3 {
                let mut mock = MockMemoryAp::with_pattern();
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len * 4]
                    .copy_from_slice(&DATA8[..len * 4]);

                let data = &DATA32[..len];
                mi.write_32(DUMMY_AP, address, data).unwrap_or_else(|_| {
                    panic!("write_32 failed, address = {}, len = {}", address, len)
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {}, len = {}",
                    address,
                    len
                );
            }
        }
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAp::with_pattern();
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for &address in &[1, 3, 127] {
            assert!(mi
                .write_32(DUMMY_AP, address, &[0xDEAD_BEEF, 0xABBA_BABE])
                .is_err());
        }
    }

    #[test]
    fn write_8() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAp::with_pattern();
                let mut mi = ADIMemoryInterface::new_mock(&mut mock);

                let mut expected = Vec::from(mi.mock_memory());
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                mi.write_8(DUMMY_AP, address, data).unwrap_or_else(|_| {
                    panic!("write_8 failed, address = {}, len = {}", address, len)
                });

                assert_eq!(
                    mi.mock_memory(),
                    expected.as_slice(),
                    "address = {}, len = {}",
                    address,
                    len
                );
            }
        }
    }

    use super::aligned_range;

    #[test]
    fn aligned_range_at_limit_does_not_panic() {
        // The aligned range for address 0xfffffff9 with length
        // 4 should not panic.

        // Not sure what the best behaviour to handle this is, but
        // for sure no panic

        let _ = aligned_range(0xfffffff9, 4);
    }
}
