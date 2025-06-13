use std::any::Any;

use zerocopy::IntoBytes;

use crate::{
    CoreStatus, MemoryInterface,
    architecture::arm::{
        ArmCommunicationInterface, ArmError, ArmDebugInterface, DapAccess, FullyQualifiedApAddress,
        ap::{
            AccessPortType, ApAccess, CSW, DataSize,
            memory_ap::{MemoryAp, MemoryApType},
        },
        communication_interface::FlushableArmAccess,
        dp::DpAccess,
        memory::ArmMemoryInterface,
    },
    probe::DebugProbeError,
};

/// Calculate the maximum number of bytes we can write starting at address
/// before we run into the 10-bit TAR autoincrement limit.
fn autoincr_max_bytes(address: u64) -> usize {
    const AUTOINCR_LIMIT: usize = 0x400;

    ((address + 1).next_multiple_of(AUTOINCR_LIMIT as _) - address) as usize
}

/// A struct to give access to a targets memory using a certain DAP.
pub(crate) struct ADIMemoryInterface<'interface, APA> {
    interface: &'interface mut APA,
    memory_ap: MemoryAp,
}

impl<'interface, APA> ADIMemoryInterface<'interface, APA>
where
    APA: ApAccess + DapAccess,
{
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(
        interface: &'interface mut APA,
        access_port_address: &FullyQualifiedApAddress,
    ) -> Result<ADIMemoryInterface<'interface, APA>, ArmError> {
        let memory_ap = MemoryAp::new(interface, access_port_address)?;
        Ok(Self {
            interface,
            memory_ap,
        })
    }
}

impl<APA> ADIMemoryInterface<'_, APA> where APA: ApAccess {}

impl<AP> MemoryInterface<ArmError> for ADIMemoryInterface<'_, AP>
where
    AP: FlushableArmAccess + ApAccess + DpAccess,
{
    /// Read a block of 64 bit words at `address`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn read_64(&mut self, mut address: u64, mut data: &mut [u64]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }

        if (address % 8) != 0 {
            return Err(ArmError::alignment_error(address, 8));
        }

        // Fall back to 32-bit accesses if 64-bit accesses are not supported.
        // In both cases the sequence of words we have to read from DRW is the same:
        // first the least significant word, then the most significant word.
        let size = match self.memory_ap.has_large_data_extension() {
            true => DataSize::U64,
            false => DataSize::U32,
        };
        self.memory_ap.try_set_datasize(self.interface, size)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 8);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.memory_ap.set_target_address(self.interface, address)?;

            let mut buf = vec![0; chunk_size * 2];
            self.memory_ap.read_data(self.interface, &mut buf)?;

            for i in 0..chunk_size {
                data[i] = buf[i * 2] as u64 | ((buf[i * 2 + 1] as u64) << 32);
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
    fn read_32(&mut self, mut address: u64, mut data: &mut [u32]) -> Result<(), ArmError> {
        if data.is_empty() {
            return Ok(());
        }

        if (address % 4) != 0 {
            return Err(ArmError::alignment_error(address, 4));
        }

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U32)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 4);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap
                .read_data(self.interface, &mut data[..chunk_size])?;

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
    fn read_16(&mut self, mut address: u64, mut data: &mut [u16]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(16));
        }

        if (address % 2) != 0 {
            return Err(ArmError::alignment_error(address, 2));
        }

        if data.is_empty() {
            return Ok(());
        }

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U16)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 2);

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            let mut values = vec![0; chunk_size];

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap.read_data(self.interface, &mut values)?;

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
    fn read_8(&mut self, mut address: u64, mut data: &mut [u8]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(8));
        }

        if data.is_empty() {
            return Ok(());
        }

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U8)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address));

            tracing::debug!(
                "Reading chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            let mut values = vec![0; chunk_size];

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap.read_data(self.interface, &mut values)?;

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

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        let len = data.len();
        if address % 4 == 0 && len % 4 == 0 {
            let mut buffer = vec![0u32; len / 4];
            self.read_32(address, &mut buffer)?;
            for (bytes, value) in data.chunks_exact_mut(4).zip(buffer.iter()) {
                bytes.copy_from_slice(&u32::to_le_bytes(*value));
            }
        } else {
            let start_address = address & !3;
            let end_address = address + (data.len() as u64);
            let end_address = end_address + (4 - (end_address & 3));
            let start_extra_count = address as usize % 4;
            let mut buffer = vec![0u32; (end_address - start_address) as usize / 4];
            self.read_32(start_address, &mut buffer)?;
            data.copy_from_slice(
                &buffer.as_bytes()[start_extra_count..start_extra_count + data.len()],
            );
        }
        Ok(())
    }

    /// Write a block of 64 bit words at `address`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be a multiple of 8.
    /// Returns `ArmError::MemoryNotAligned` if this does not hold true.
    fn write_64(&mut self, mut address: u64, mut data: &[u64]) -> Result<(), ArmError> {
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
        let size = match self.memory_ap.has_large_data_extension() {
            true => DataSize::U64,
            false => DataSize::U32,
        };
        self.memory_ap.try_set_datasize(self.interface, size)?;

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
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap.write_data(self.interface, &values)?;

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
    fn write_32(&mut self, mut address: u64, mut data: &[u32]) -> Result<(), ArmError> {
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

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U32)?;

        while !data.is_empty() {
            let chunk_size = data.len().min(autoincr_max_bytes(address) / 4);

            tracing::debug!(
                "Writing chunk with len {} at address {:#08x}",
                chunk_size,
                address
            );

            // autoincrement is limited to the 10 lowest bits, so write TAR every time.
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap
                .write_data(self.interface, &data[..chunk_size])?;

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
    fn write_16(&mut self, mut address: u64, mut data: &[u16]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
            return Err(ArmError::UnsupportedTransferWidth(16));
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

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U16)?;

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
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap.write_data(self.interface, &values)?;

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
    fn write_8(&mut self, mut address: u64, mut data: &[u8]) -> Result<(), ArmError> {
        if self.memory_ap.supports_only_32bit_data_size() {
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

        self.memory_ap
            .try_set_datasize(self.interface, DataSize::U8)?;

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
            self.memory_ap.set_target_address(self.interface, address)?;
            self.memory_ap.write_data(self.interface, &values)?;

            address = address
                .checked_add(chunk_size as u64)
                .ok_or(ArmError::OutOfBounds)?;
            data = &data[chunk_size..];
        }

        tracing::debug!("Finished writing block");

        Ok(())
    }

    /// Flushes any pending commands when the underlying probe interface implements command queuing.
    fn flush(&mut self) -> Result<(), ArmError> {
        self.interface.flush()
    }

    /// True if the memory ap supports 64 bit accesses which might be more efficient than issuing
    /// two 32bit transaction on the device’s memory bus.
    fn supports_native_64bit_access(&mut self) -> bool {
        self.memory_ap.has_large_data_extension()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(!self.memory_ap.supports_only_32bit_data_size())
    }
}

impl<APA> ArmMemoryInterface for ADIMemoryInterface<'_, APA>
where
    APA: std::any::Any + FlushableArmAccess + ApAccess + DpAccess + ArmDebugInterface,
{
    fn base_address(&mut self) -> Result<u64, ArmError> {
        self.memory_ap.base_address(self.interface)
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.memory_ap.ap_address().clone()
    }

    fn get_swd_sequence(
        &mut self,
    ) -> Result<
        &mut dyn crate::architecture::arm::communication_interface::SwdSequence,
        DebugProbeError,
    > {
        Ok(self.interface)
    }

    fn get_arm_probe_interface(
        &mut self,
    ) -> Result<&mut dyn crate::architecture::arm::ArmDebugInterface, DebugProbeError> {
        Ok(self.interface)
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        Ok(self.interface)
    }

    fn generic_status(&mut self) -> Result<CSW, ArmError> {
        // TODO: This assumes that the base type is `ArmCommunicationInterface`,
        // which will fail if something else implements `ADIMemoryInterface`.
        let Some(iface) =
            (self.interface as &mut dyn Any).downcast_mut::<ArmCommunicationInterface>()
        else {
            return Err(ArmError::Probe(DebugProbeError::Other(
                "Not an ArmCommunicationInterface".to_string(),
            )));
        };

        self.memory_ap.generic_status(iface)
    }

    fn update_core_status(&mut self, state: CoreStatus) {
        // TODO: This assumes that the base type is `ArmCommunicationInterface`,
        // which will fail if something else implements `ADIMemoryInterface`.
        let Some(iface) =
            (self.interface as &mut dyn Any).downcast_mut::<ArmCommunicationInterface>()
        else {
            return;
        };

        iface.probe_mut().core_status_notification(state).ok();
    }
}

#[cfg(test)]
mod tests {
    use scroll::Pread;
    use test_log::test;

    use crate::{
        MemoryInterface,
        architecture::arm::{
            FullyQualifiedApAddress, ap::memory_ap::mock::MockMemoryAp, memory::ADIMemoryInterface,
        },
    };

    impl<'interface> ADIMemoryInterface<'interface, MockMemoryAp> {
        /// Creates a new MemoryInterface for given AccessPort.
        fn new_mock(
            mock: &'interface mut MockMemoryAp,
        ) -> ADIMemoryInterface<'interface, MockMemoryAp> {
            Self::new(mock, &FullyQualifiedApAddress::v1_with_default_dp(0)).unwrap()
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

    #[test]
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
    fn read() {
        let mut mock = MockMemoryAp::with_pattern_and_size(256);
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);
        let mut mi = ADIMemoryInterface::new_mock(&mut mock);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                mi.read(address, &mut data)
                    .unwrap_or_else(|_| panic!("read failed, address = {address}, len = {len}"));

                assert_eq!(
                    &DATA8[address as usize..address as usize + len],
                    data.as_slice(),
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
