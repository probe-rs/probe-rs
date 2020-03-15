use super::super::ap::{
    mock::MockMemoryAP, APAccess, APRegister, AccessPortError, AddressIncrement, DataSize,
    MemoryAP, CSW, DRW, TAR,
};
use crate::architecture::arm::{
    dp::{DPAccess, RdBuff},
    ArmCommunicationInterface,
};
use crate::{CommunicationInterface, Error, MemoryInterface};
use scroll::{Pread, LE};

/// A struct to give access to a targets memory using a certain DAP.
pub struct ADIMemoryInterface<AP>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>
        + DPAccess,
{
    interface: AP,
    access_port: MemoryAP,
}

impl ADIMemoryInterface<ArmCommunicationInterface> {
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(
        interface: ArmCommunicationInterface,
        access_port_number: impl Into<MemoryAP>,
    ) -> Self {
        Self {
            interface,
            access_port: access_port_number.into(),
        }
    }
}

impl ADIMemoryInterface<MockMemoryAP> {
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(mock: MockMemoryAP, access_port_number: impl Into<MemoryAP>) -> Self {
        Self {
            interface: mock,
            access_port: access_port_number.into(),
        }
    }
}

impl<AP> ADIMemoryInterface<AP>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>
        + DPAccess,
{
    /// Build the correct CSW register for a memory access
    ///
    /// Currently, only AMBA AHB Access is supported.
    fn build_csw_register(&self, data_size: DataSize) -> CSW {
        // The CSW Register is set for an AMBA AHB Acccess, according to
        // the ARM Debug Interface Architecture Specification.
        //
        // The PROT bits are set as follows:
        //  BIT[30]              = 1  - Should be One, otherwise unpredictable
        //  MasterType, bit [29] = 1  - Access as default AHB Master
        //  HPROT[4]             = 0  - Non-allocating access
        //
        // The CACHE bits are set for the following AHB access:
        //   HPROT[0] == 1   - data           access
        //   HPROT[1] == 1   - privileged     access
        //   HPROT[2] == 0   - non-cacheable  access
        //   HPROT[3] == 0   - non-bufferable access

        CSW {
            PROT: 0b110,
            CACHE: 0b11,
            AddrInc: AddressIncrement::Single,
            SIZE: data_size,
            ..Default::default()
        }
    }

    /// Read a 32 bit register on the given AP.
    fn read_ap_register<R>(&mut self, register: R) -> Result<R, AccessPortError>
    where
        R: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, R>,
    {
        self.interface
            .read_ap_register(self.access_port, register)
            .or_else(|_| Err(AccessPortError::register_read_error::<R>()))
    }

    /// Read multiple 32 bit values from the same
    /// register on the given AP.
    fn read_ap_register_repeated<R>(
        &mut self,
        register: R,
        values: &mut [u32],
    ) -> Result<(), AccessPortError>
    where
        R: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, R>,
    {
        self.interface
            .read_ap_register_repeated(self.access_port, register, values)
            .or_else(|_| Err(AccessPortError::register_read_error::<R>()))
    }

    /// Write a 32 bit register on the given AP.
    fn write_ap_register<R>(&mut self, register: R) -> Result<(), AccessPortError>
    where
        R: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, R>,
    {
        self.interface
            .write_ap_register(self.access_port, register)
            .or_else(|_| Err(AccessPortError::register_write_error::<R>()))
    }

    /// Write multiple 32 bit values to the same
    /// register on the given AP.
    fn write_ap_register_repeated<R>(
        &mut self,
        register: R,
        values: &[u32],
    ) -> Result<(), AccessPortError>
    where
        R: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, R>,
    {
        self.interface
            .write_ap_register_repeated(self.access_port, register, values)
            .or_else(|_| Err(AccessPortError::register_write_error::<R>()))
    }

    /// Read a 32bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read32(&mut self, address: u32) -> Result<u32, AccessPortError> {
        if (address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        let csw = self.build_csw_register(DataSize::U32);

        let tar = TAR { address };
        self.write_ap_register(csw)?;
        self.write_ap_register(tar)?;
        let result = self.read_ap_register(DRW::default())?;

        Ok(result.data)
    }

    /// Read an 8bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read8(&mut self, address: u32) -> Result<u8, AccessPortError> {
        let pre_bytes = ((4 - (address % 4)) % 4) as usize;
        let aligned_addr = address - (address % 4);

        let result = self.read32(aligned_addr)?;

        dbg!(pre_bytes);

        Ok(match pre_bytes {
            3 => ((result >> 8) & 0xff) as u8,
            2 => ((result >> 16) & 0xff) as u8,
            1 => ((result >> 24) & 0xff) as u8,
            0 => (result & 0xff) as u8,
            _ => panic!("This case cannot happen ever. This must be a bug. Please report it."),
        })
    }

    /// Read a block of words of the size defined by S at `addr`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read_block32(
        &mut self,
        start_address: u32,
        data: &mut [u32],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        if (start_address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        // Second we read in 32 bit reads until we have less than 32 bits left to read.
        let csw = self.build_csw_register(DataSize::U32);
        self.write_ap_register(csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(tar)?;

        // figure out how many words we can write before the
        // data overflows

        // maximum chunk size
        let max_chunk_size_bytes = 0x400;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len() * 4,
        );

        let mut data_offset = 0;

        log::debug!(
            "Read first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_words = first_chunk_size_bytes / 4;

        self.read_ap_register_repeated(
            DRW { data: 0 },
            &mut data[data_offset..first_chunk_size_words],
        )?;

        remaining_data_len -= first_chunk_size_words;
        address += (4 * first_chunk_size_words) as u32;
        data_offset += first_chunk_size_words;

        while remaining_data_len > 0 {
            // the autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows
            let tar = TAR { address };
            self.write_ap_register(tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Reading chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.read_ap_register_repeated(
                DRW { data: 0 },
                &mut data[data_offset..(data_offset + next_chunk_size_words)],
            )?;

            remaining_data_len -= next_chunk_size_words;
            address += (4 * next_chunk_size_words) as u32;
            data_offset += next_chunk_size_words;
        }

        log::debug!("Finished reading block");

        Ok(())
    }

    pub fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        // Round start address down to the nearest multiple of 4
        let aligned_addr = address - (address % 4);

        let unaligned_end_addr = address
            .checked_add(data.len() as u32)
            .ok_or(AccessPortError::OutOfBoundsError)?;

        // Round end address up to the nearest multiple of 4
        let aligned_end_addr = unaligned_end_addr + ((4 - (unaligned_end_addr % 4)) % 4);

        // Read aligned block of 32-bit words
        let mut buf32 = vec![0u32; ((aligned_end_addr - aligned_addr) / 4) as usize];
        self.read_block32(aligned_addr, &mut buf32)?;

        // Convert 32-bit words to bytes
        let mut buf8 = vec![0u8; (aligned_end_addr - aligned_addr) as usize];
        for i in 0..buf32.len() {
            buf8[i * 4..(i + 1) * 4].copy_from_slice(&buf32[i].to_le_bytes());
        }

        // Copy relevant part of aligned block to output data
        let start = (address - aligned_addr) as usize;
        data.copy_from_slice(&buf8[start..start + data.len()]);

        Ok(())
    }

    /// Write a 32bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write32(&mut self, address: u32, data: u32) -> Result<(), AccessPortError> {
        if (address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };
        let tar = TAR { address };
        self.write_ap_register(csw)?;
        self.write_ap_register(tar)?;
        self.write_ap_register(drw)?;

        // Ensure the write is actually performed
        let _: RdBuff = self
            .interface
            .read_dp_register()
            .map_err(|_| AccessPortError::InvalidAccessPortNumber)?;

        Ok(())
    }

    /// Write an 8bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write8(&mut self, address: u32, data: u8) -> Result<(), AccessPortError> {
        let pre_bytes = (address % 4) as usize;
        let aligned_addr = address - (address % 4);

        let before = self.read32(aligned_addr)?;
        let data_t = before & !(0xFF << (pre_bytes * 8));
        let data = data_t | (u32::from(data) << (pre_bytes * 8));

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };
        let tar = TAR {
            address: aligned_addr,
        };
        self.write_ap_register(csw)?;
        self.write_ap_register(tar)?;
        self.write_ap_register(drw)?;

        // Ensure the last write is actually performed
        let _: RdBuff = self.interface.read_dp_register()?;

        Ok(())
    }

    /// Write a block of 32bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_block32(
        &mut self,
        start_address: u32,
        data: &[u32],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        if (start_address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        log::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len() * 4,
            start_address
        );

        // Second we write in 32 bit reads until we have less than 32 bits left to write.
        let csw = self.build_csw_register(DataSize::U32);

        self.write_ap_register(csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(tar)?;

        // figure out how many words we can write before the
        // data overflows

        // maximum chunk size
        let max_chunk_size_bytes = 0x400_usize;

        let mut remaining_data_len = data.len();

        let first_chunk_size_bytes = std::cmp::min(
            max_chunk_size_bytes - (address as usize % max_chunk_size_bytes),
            data.len() * 4,
        );

        let mut data_offset = 0;

        log::debug!(
            "Write first block with len {} at address {:#08x}",
            first_chunk_size_bytes,
            address
        );

        let first_chunk_size_words = first_chunk_size_bytes / 4;

        self.write_ap_register_repeated(
            DRW { data: 0 },
            &data[data_offset..first_chunk_size_words],
        )?;

        remaining_data_len -= first_chunk_size_words;
        address += (4 * first_chunk_size_words) as u32;
        data_offset += first_chunk_size_words;

        while remaining_data_len > 0 {
            // the autoincrement is limited to the 10 lowest bits so we need to write the address
            // every time it overflows
            let tar = TAR { address };
            self.write_ap_register(tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Writing chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.write_ap_register_repeated(
                DRW { data: 0 },
                &data[data_offset..(data_offset + next_chunk_size_words)],
            )?;

            remaining_data_len -= next_chunk_size_words;
            address += (4 * next_chunk_size_words) as u32;
            data_offset += next_chunk_size_words;
        }

        // Ensure the last write is actually performed
        let _: RdBuff = self.interface.read_dp_register()?;

        log::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 8bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_block8(&mut self, address: u32, data: &[u8]) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        let pre_bytes = usize::min(data.len(), ((4 - (address % 4)) % 4) as usize);
        let aligned_address = address + pre_bytes as u32;
        let post_bytes = (data.len() - pre_bytes) % 4;

        if pre_bytes != 0 {
            let pre_address = aligned_address - 4;
            let mut pre_data = self.read32(pre_address)?;
            for (i, shift) in (4 - pre_bytes..4).enumerate() {
                pre_data &= !(0xFF << (shift * 8));
                pre_data |= u32::from(data[i]) << (shift * 8);
            }

            self.write32(pre_address, pre_data)?;
        }

        self.write_block32(
            aligned_address,
            data[pre_bytes..data.len() - post_bytes]
                .chunks(4)
                .map(|c| {
                    c.pread_with::<u32>(0, LE)
                        .expect("This is a bug. Please report it.")
                })
                .collect::<Vec<_>>()
                .as_slice(),
        )?;

        if post_bytes != 0 {
            let post_address = address + (data.len() - post_bytes) as u32;
            let mut post_data = self.read32(post_address)?;

            dbg!(post_bytes);
            for shift in 0..post_bytes {
                post_data &= !(0xFF << (shift * 8));
                post_data |= u32::from(data[data.len() - post_bytes + shift]) << (shift * 8);
            }

            self.write32(post_address, post_data)?;
        }

        Ok(())
    }
}

impl<AP> MemoryInterface for ADIMemoryInterface<AP>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>
        + DPAccess,
{
    fn read32(&mut self, address: u32) -> Result<u32, Error> {
        ADIMemoryInterface::read32(self, address).map_err(Error::architecture_specific)
    }

    fn read8(&mut self, address: u32) -> Result<u8, Error> {
        ADIMemoryInterface::read8(self, address).map_err(Error::architecture_specific)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        ADIMemoryInterface::read_block32(self, address, data).map_err(Error::architecture_specific)
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        ADIMemoryInterface::read_block8(self, address, data).map_err(Error::architecture_specific)
    }

    fn write32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        ADIMemoryInterface::write32(self, address, data).map_err(Error::architecture_specific)
    }

    fn write8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        ADIMemoryInterface::write8(self, address, data).map_err(Error::architecture_specific)
    }

    fn write_block32(&mut self, address: u32, data: &[u32]) -> Result<(), Error> {
        ADIMemoryInterface::write_block32(self, address, data).map_err(Error::architecture_specific)
    }

    fn write_block8(&mut self, address: u32, data: &[u8]) -> Result<(), Error> {
        ADIMemoryInterface::write_block8(self, address, data).map_err(Error::architecture_specific)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::ap::mock::MockMemoryAP;
    use super::ADIMemoryInterface;

    #[test]
    fn read_u32() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let read = mi.read32(0);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xDEAD_BEEF);
    }

    #[test]
    #[ignore]
    fn read_u16() {
        // let mut mock = MockMemoryAP::default();
        // mock.data[0] = 0xEF;
        // mock.data[1] = 0xBE;
        // mock.data[2] = 0xAD;
        // mock.data[3] = 0xDE;
        // let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        // let read: Result<u16, _> = mi.read(0);
        // let read2: Result<u16, _> = mi.read(2);
        // debug_assert!(read.is_ok());
        // debug_assert_eq!(read.unwrap(), 0xBEEF);
        // debug_assert_eq!(read2.unwrap(), 0xDEAD);
    }

    #[test]
    fn read_u8() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let read = mi.read8(0);
        let read2 = mi.read8(1);
        let read3 = mi.read8(2);
        let read4 = mi.read8(3);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xEF);
        debug_assert_eq!(read2.unwrap(), 0xBE);
        debug_assert_eq!(read3.unwrap(), 0xAD);
        debug_assert_eq!(read4.unwrap(), 0xDE);
    }

    #[test]
    fn write_u32() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi.write32(0, 0xDEAD_BEEF as u32).is_ok());
        let buf = &mut [0; 4];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(buf, &[0xEF, 0xBE, 0xAD, 0xDE])
    }

    #[test]
    #[ignore]
    fn write_u16() {
        // let mut mock = MockMemoryAP::default();
        // let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        // debug_assert!(mi.write(0, 0xBEEF as u16).is_ok());
        // debug_assert!(mi.write(2, 0xDEAD as u16).is_ok());
        // debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u8() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi.write8(0, 0xEF as u8).is_ok());
        debug_assert!(mi.write8(1, 0xBE as u8).is_ok());
        debug_assert!(mi.write8(2, 0xAD as u8).is_ok());
        debug_assert!(mi.write8(3, 0xDE as u8).is_ok());
        let buf = &mut [0; 4];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(buf, &[0xEF, 0xBE, 0xAD, 0xDE])
    }

    #[test]
    fn read_block_u32() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u32; 2];
        let read = mi.read_block32(0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xDEAD_BEEF, 0xABBA_BABE]);
    }

    #[test]
    fn read_block_u32_only_1_word() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u32; 1];
        let read = mi.read_block32(0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xDEAD_BEEF]);
    }

    #[test]
    fn read_block_u32_unaligned_should_error() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u32; 4];
        debug_assert!(mi.read_block32(1, &mut data).is_err());
        debug_assert!(mi.read_block32(127, &mut data).is_err());
        debug_assert!(mi.read_block32(3, &mut data).is_err());
    }

    /*

    #[test]
    fn read_block_u16() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block32(0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xBEEF, 0xDEAD, 0xBABE, 0xABBA]);
    }

    #[test]
    fn read_block_u16_unaligned() {
        let mut mock = MockMemoryAP::default();
        mock.data[2] = 0xEF;
        mock.data[3] = 0xBE;
        mock.data[4] = 0xAD;
        mock.data[5] = 0xDE;
        mock.data[6] = 0xBE;
        mock.data[7] = 0xBA;
        mock.data[8] = 0xBA;
        mock.data[9] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block32(2, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xBEEF, 0xDEAD, 0xBABE, 0xABBA]);
    }

    #[test]
    fn read_block_u16_unaligned_should_error() {
        let mut mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u16; 4];
        debug_assert!(mi.read_block32(1, &mut data).is_err());
        debug_assert!(mi.read_block32(127, &mut data).is_err());
        debug_assert!(mi.read_block32(3, &mut data).is_err());
    }

    */

    #[test]
    fn read_block_u8() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]);
    }

    #[test]
    fn read_block_u8_unaligned() {
        let mut mock = MockMemoryAP::default();
        mock.data[1] = 0xEF;
        mock.data[2] = 0xBE;
        mock.data[3] = 0xAD;
        mock.data[4] = 0xDE;
        mock.data[5] = 0xBE;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xBA;
        mock.data[8] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(1, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]);
    }

    #[test]
    fn read_block_u8_unaligned2() {
        let mut mock = MockMemoryAP::default();
        mock.data[3] = 0xEF;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xAD;
        mock.data[6] = 0xDE;
        mock.data[7] = 0xBE;
        mock.data[8] = 0xBA;
        mock.data[9] = 0xBA;
        mock.data[10] = 0xAB;
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(3, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]);
    }

    #[test]
    fn write_block_u32() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi
            .write_block32(0, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_ok());
        let buf = &mut [0; 8];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(buf, &[0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB])
    }

    #[test]
    fn write_block_u32_only_1_word() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi.write_block32(0, &([0xDEAD_BEEF] as [u32; 1])).is_ok());
        let buf = &mut [0; 1];
        debug_assert!(mi.read_block32(0, buf).is_ok());
        debug_assert_eq!(buf, &[0xDEAD_BEEFu32])
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi
            .write_block32(1, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
        debug_assert!(mi
            .write_block32(127, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
        debug_assert!(mi
            .write_block32(3, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
    }

    #[test]
    #[ignore]
    fn write_block_u16() {
        // let mut mock = MockMemoryAP::default();
        // let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        // debug_assert!(mi.write_block(0, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        // debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    #[ignore]
    fn write_block_u16_unaligned2() {
        // let mut mock = MockMemoryAP::default();
        // let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        // debug_assert!(mi.write_block(2, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        // debug_assert_eq!(mock.data[0..10], [0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    #[ignore]
    fn write_block_u16_unaligned_should_error() {
        // let mut mock = MockMemoryAP::default();
        // let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        // debug_assert!(mi.write_block(1, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        // debug_assert!(mi.write_block(127, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        // debug_assert!(mi.write_block(3, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
    }

    #[test]
    fn write_block_u8() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi
            .write_block8(
                0,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        let buf = &mut [0; 8];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(buf, &[0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB])
    }

    #[test]
    fn write_block_u8_unaligned() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi
            .write_block8(
                3,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        let buf = &mut [0; 11];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(
            buf,
            &[0x00, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]
        )
    }

    #[test]
    fn write_block_u8_unaligned2() {
        let mock = MockMemoryAP::default();
        let mut mi = ADIMemoryInterface::<MockMemoryAP>::new(mock, 0x0);
        debug_assert!(mi
            .write_block8(
                1,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        let buf = &mut [0; 9];
        debug_assert!(mi.read_block8(0, buf).is_ok());
        debug_assert_eq!(buf, &[0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB])
    }
}
