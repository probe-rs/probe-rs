/// Memory access according to ARM Debug Interface specification v5.0
use crate::coresight::access_ports::{
    memory_ap::{AddressIncrement, DataSize, MemoryAP, CSW, DRW, TAR},
    APRegister, AccessPortError,
};
use crate::coresight::ap_access::APAccess;
use scroll::Pread;

/// A struct to give access to a targets memory using a certain DAP.
pub struct ADIMemoryInterface {
    access_port: MemoryAP,
}

pub fn bytes_to_transfer_size(bytes: u8) -> DataSize {
    if bytes == 1 {
        DataSize::U8
    } else if bytes == 2 {
        DataSize::U16
    } else if bytes == 4 {
        DataSize::U32
    } else if bytes == 8 {
        DataSize::U64
    } else if bytes == 16 {
        DataSize::U128
    } else if bytes == 32 {
        DataSize::U256
    } else {
        DataSize::U32
    }
}

impl ADIMemoryInterface {
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(access_port_number: u8) -> Self {
        Self {
            access_port: MemoryAP::new(access_port_number),
        }
    }

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
    fn read_ap_register<REGISTER, AP>(
        &self,
        debug_port: &mut AP,
        register: REGISTER,
    ) -> Result<REGISTER, AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>,
    {
        debug_port
            .read_ap_register(self.access_port, register)
            .or_else(|_| Err(AccessPortError::register_read_error::<REGISTER>()))
    }

    /// Read multiple 32 bit values from the same
    /// register on the given AP.
    fn read_ap_register_repeated<REGISTER, AP>(
        &self,
        debug_port: &mut AP,
        register: REGISTER,
        values: &mut [u32],
    ) -> Result<(), AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>,
    {
        debug_port
            .read_ap_register_repeated(self.access_port, register, values)
            .or_else(|_| Err(AccessPortError::register_read_error::<REGISTER>()))
    }

    /// Write a 32 bit register on the given AP.
    fn write_ap_register<REGISTER, AP>(
        &self,
        debug_port: &mut AP,
        register: REGISTER,
    ) -> Result<(), AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>,
    {
        debug_port
            .write_ap_register(self.access_port, register)
            .or_else(|_| Err(AccessPortError::register_write_error::<REGISTER>()))
    }

    /// Write multiple 32 bit values to the same
    /// register on the given AP.
    fn write_ap_register_repeated<REGISTER, AP>(
        &self,
        debug_port: &mut AP,
        register: REGISTER,
        values: &[u32],
    ) -> Result<(), AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>,
    {
        debug_port
            .write_ap_register_repeated(self.access_port, register, values)
            .or_else(|_| Err(AccessPortError::register_write_error::<REGISTER>()))
    }

    /// Read a 32bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read32<AP>(&self, debug_port: &mut AP, address: u32) -> Result<u32, AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if (address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        let csw = self.build_csw_register(DataSize::U32);

        let tar = TAR { address };
        self.write_ap_register(debug_port, csw)?;
        self.write_ap_register(debug_port, tar)?;
        let result = self.read_ap_register(debug_port, DRW::default())?;

        Ok(result.data)
    }

    /// Read an 8bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read8<AP>(&self, debug_port: &mut AP, address: u32) -> Result<u8, AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        let pre_bytes = ((4 - (address % 4)) % 4) as usize;
        let aligned_addr = address - (address % 4);

        let result = self.read32(debug_port, aligned_addr)?;

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
    pub fn read_block32<AP>(
        &self,
        debug_port: &mut AP,
        start_address: u32,
        data: &mut [u32],
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if data.len() == 0 {
            return Ok(());
        }

        if (start_address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        // Second we read in 32 bit reads until we have less than 32 bits left to read.
        let csw = self.build_csw_register(DataSize::U32);
        self.write_ap_register(debug_port, csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(debug_port, tar)?;

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
            debug_port,
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
            self.write_ap_register(debug_port, tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Reading chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.read_ap_register_repeated(
                debug_port,
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

    pub fn read_block8<AP>(
        &self,
        debug_port: &mut AP,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if data.len() == 0 {
            return Ok(());
        }

        let pre_bytes = ((4 - (address % 4)) % 4) as usize;

        let aligned_addr = address - (address % 4);
        let unaligned_end_addr = address
            .checked_add(data.len() as u32)
            .ok_or(AccessPortError::OutOfBoundsError)?;

        let aligned_end_addr = if unaligned_end_addr % 4 != 0 {
            (unaligned_end_addr - (unaligned_end_addr % 4)) + 4
        } else {
            unaligned_end_addr
        };

        let post_bytes = ((4 - (aligned_end_addr - unaligned_end_addr)) % 4) as usize;

        let aligned_read_len = (aligned_end_addr - aligned_addr) as usize;

        let mut aligned_data_len = aligned_read_len;

        if pre_bytes > 0 {
            aligned_data_len -= 4;
        }

        if post_bytes > 0 {
            aligned_data_len -= 4;
        }

        assert_eq!(pre_bytes + aligned_data_len + post_bytes, data.len());
        // TODO: fix;
        // assert_eq!(aligned_read_len - pre_bytes - post_bytes, data.len());

        let mut buff = vec![0u32; (aligned_read_len / 4) as usize];

        self.read_block32(debug_port, aligned_addr, &mut buff)?;

        match pre_bytes {
            3 => {
                data[0] = ((buff[0] >> 8) & 0xff) as u8;
                data[1] = ((buff[0] >> 16) & 0xff) as u8;
                data[2] = ((buff[0] >> 24) & 0xff) as u8;
            }
            2 => {
                data[0] = ((buff[0] >> 16) & 0xff) as u8;
                data[1] = ((buff[0] >> 24) & 0xff) as u8;
            }
            1 => {
                data[0] = ((buff[0] >> 24) & 0xff) as u8;
            }
            _ => (),
        };

        if aligned_read_len > 0 {
            let aligned_data =
                &mut data[(pre_bytes as usize)..((pre_bytes + aligned_data_len) as usize)];

            let word_offset_start = if pre_bytes > 0 { 1 } else { 0 } as usize;

            for (i, word) in buff[word_offset_start..(word_offset_start + aligned_data_len / 4)]
                .iter()
                .enumerate()
            {
                aligned_data[i * 4] = (word & 0xff) as u8;
                aligned_data[i * 4 + 1] = ((word >> 8) & 0xffu32) as u8;
                aligned_data[i * 4 + 2] = ((word >> 16) & 0xffu32) as u8;
                aligned_data[i * 4 + 3] = ((word >> 24) & 0xffu32) as u8;
            }
        }

        match post_bytes {
            1 => {
                data[data.len() - 1] = (buff[buff.len() - 1] & 0xff) as u8;
            }
            2 => {
                data[data.len() - 2] = (buff[buff.len() - 1] & 0xff) as u8;
                data[data.len() - 1] = ((buff[buff.len() - 1] >> 8) & 0xff) as u8;
            }
            3 => {
                data[data.len() - 3] = (buff[buff.len() - 1] & 0xff) as u8;
                data[data.len() - 2] = ((buff[buff.len() - 1] >> 8) & 0xff) as u8;
                data[data.len() - 1] = ((buff[buff.len() - 1] >> 16) & 0xff) as u8;
            }
            _ => (),
        }

        Ok(())
    }

    /// Write a 32bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write32<AP>(
        &self,
        debug_port: &mut AP,
        address: u32,
        data: u32,
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if (address % 4) != 0 {
            return Err(AccessPortError::MemoryNotAligned);
        }

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };
        let tar = TAR { address };
        self.write_ap_register(debug_port, csw)?;
        self.write_ap_register(debug_port, tar)?;
        self.write_ap_register(debug_port, drw)?;
        Ok(())
    }

    /// Write an 8bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write8<AP>(
        &self,
        debug_port: &mut AP,
        address: u32,
        data: u8,
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        let pre_bytes = (address % 4) as usize;
        let aligned_addr = address - (address % 4);

        let before = self.read32(debug_port, aligned_addr)?;
        let data_t = before & !(0xFF << (pre_bytes * 8));
        let data = data_t | (u32::from(data) << (pre_bytes * 8));

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };
        let tar = TAR {
            address: aligned_addr,
        };
        self.write_ap_register(debug_port, csw)?;
        self.write_ap_register(debug_port, tar)?;
        self.write_ap_register(debug_port, drw)?;
        Ok(())
    }

    /// Write a block of 32bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_block32<AP>(
        &self,
        debug_port: &mut AP,
        start_address: u32,
        data: &[u32],
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if data.len() == 0 {
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

        self.write_ap_register(debug_port, csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(debug_port, tar)?;

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
            debug_port,
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
            self.write_ap_register(debug_port, tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Writing chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.write_ap_register_repeated(
                debug_port,
                DRW { data: 0 },
                &data[data_offset..(data_offset + next_chunk_size_words)],
            )?;

            remaining_data_len -= next_chunk_size_words;
            address += (4 * next_chunk_size_words) as u32;
            data_offset += next_chunk_size_words;
        }

        log::debug!("Finished writing block");

        Ok(())
    }

    /// Write a block of 8bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_block8<AP>(
        &self,
        debug_port: &mut AP,
        address: u32,
        data: &[u8],
    ) -> Result<(), AccessPortError>
    where
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>,
    {
        if data.len() == 0 {
            return Ok(());
        }

        let pre_bytes = usize::min(data.len(), ((4 - (address % 4)) % 4) as usize);
        let aligned_address = address + pre_bytes as u32;
        let pre_address = aligned_address - 4;
        let post_bytes = (data.len() - pre_bytes) % 4;
        let post_address = address + (data.len() - post_bytes) as u32;

        if pre_bytes != 0 {
            let mut pre_data = self.read32(debug_port, pre_address)?;
            for (i, shift) in (4 - pre_bytes..4).enumerate() {
                pre_data &= !(0xFF << (shift * 8));
                pre_data |= u32::from(data[i]) << (shift * 8);
            }

            self.write32(debug_port, pre_address, pre_data)?;
        }

        self.write_block32(
            debug_port,
            aligned_address,
            data[pre_bytes..data.len() - post_bytes]
                .chunks(4)
                .map(|c| c.pread::<u32>(0).expect("This is a bug. Please report it."))
                .collect::<Vec<_>>()
                .as_slice(),
        )?;

        if post_bytes != 0 {
            let mut post_data = self.read32(debug_port, post_address)?;

            dbg!(post_bytes);
            for shift in 0..post_bytes {
                post_data &= !(0xFF << (shift * 8));
                post_data |= u32::from(data[data.len() - post_bytes + shift]) << (shift * 8);
            }

            self.write32(debug_port, post_address, post_data)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ADIMemoryInterface;
    use crate::coresight::access_ports::memory_ap::mock::MockMemoryAP;

    #[test]
    fn read_u32() {
        let mut mock = MockMemoryAP::default();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mi = ADIMemoryInterface::new(0x0);
        let read = mi.read32(&mut mock, 0);
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
        // let mi = ADIMemoryInterface::new(0x0);
        // let read: Result<u16, _> = mi.read(&mut mock, 0);
        // let read2: Result<u16, _> = mi.read(&mut mock, 2);
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
        let mi = ADIMemoryInterface::new(0x0);
        let read = mi.read8(&mut mock, 0);
        let read2 = mi.read8(&mut mock, 1);
        let read3 = mi.read8(&mut mock, 2);
        let read4 = mi.read8(&mut mock, 3);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xEF);
        debug_assert_eq!(read2.unwrap(), 0xBE);
        debug_assert_eq!(read3.unwrap(), 0xAD);
        debug_assert_eq!(read4.unwrap(), 0xDE);
    }

    #[test]
    fn write_u32() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi.write32(&mut mock, 0, 0xDEAD_BEEF as u32).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    #[ignore]
    fn write_u16() {
        // let mut mock = MockMemoryAP::default();
        // let mi = ADIMemoryInterface::new(0x0);
        // debug_assert!(mi.write(&mut mock, 0, 0xBEEF as u16).is_ok());
        // debug_assert!(mi.write(&mut mock, 2, 0xDEAD as u16).is_ok());
        // debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u8() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi.write8(&mut mock, 0, 0xEF as u8).is_ok());
        debug_assert!(mi.write8(&mut mock, 1, 0xBE as u8).is_ok());
        debug_assert!(mi.write8(&mut mock, 2, 0xAD as u8).is_ok());
        debug_assert!(mi.write8(&mut mock, 3, 0xDE as u8).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u32; 2];
        let read = mi.read_block32(&mut mock, 0, &mut data);
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u32; 1];
        let read = mi.read_block32(&mut mock, 0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xDEAD_BEEF]);
    }

    #[test]
    fn read_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u32; 4];
        debug_assert!(mi.read_block32(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block32(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block32(&mut mock, 3, &mut data).is_err());
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block32(&mut mock, 0, &mut data);
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block32(&mut mock, 2, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xBEEF, 0xDEAD, 0xBABE, 0xABBA]);
    }

    #[test]
    fn read_block_u16_unaligned_should_error() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        debug_assert!(mi.read_block32(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block32(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block32(&mut mock, 3, &mut data).is_err());
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(&mut mock, 0, &mut data);
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(&mut mock, 1, &mut data);
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
        let mi = ADIMemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block8(&mut mock, 3, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]);
    }

    #[test]
    fn write_block_u32() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block32(&mut mock, 0, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_ok());
        debug_assert_eq!(
            mock.data[0..8],
            [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]
        );
    }

    #[test]
    fn write_block_u32_only_1_word() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block32(&mut mock, 0, &([0xDEAD_BEEF] as [u32; 1]))
            .is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block32(&mut mock, 1, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
        debug_assert!(mi
            .write_block32(&mut mock, 127, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
        debug_assert!(mi
            .write_block32(&mut mock, 3, &([0xDEAD_BEEF, 0xABBA_BABE] as [u32; 2]))
            .is_err());
    }

    #[test]
    #[ignore]
    fn write_block_u16() {
        // let mut mock = MockMemoryAP::default();
        // let mi = ADIMemoryInterface::new(0x0);
        // debug_assert!(mi.write_block(&mut mock, 0, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        // debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    #[ignore]
    fn write_block_u16_unaligned2() {
        // let mut mock = MockMemoryAP::default();
        // let mi = ADIMemoryInterface::new(0x0);
        // debug_assert!(mi.write_block(&mut mock, 2, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        // debug_assert_eq!(mock.data[0..10], [0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    #[ignore]
    fn write_block_u16_unaligned_should_error() {
        // let mut mock = MockMemoryAP::default();
        // let mi = ADIMemoryInterface::new(0x0);
        // debug_assert!(mi.write_block(&mut mock, 1, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        // debug_assert!(mi.write_block(&mut mock, 127, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        // debug_assert!(mi.write_block(&mut mock, 3, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
    }

    #[test]
    fn write_block_u8() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block8(
                &mut mock,
                0,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        debug_assert_eq!(
            mock.data[0..8],
            [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]
        );
    }

    #[test]
    fn write_block_u8_unaligned() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block8(
                &mut mock,
                3,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        debug_assert_eq!(
            mock.data[0..11],
            [0x00, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]
        );
    }

    #[test]
    fn write_block_u8_unaligned2() {
        let mut mock = MockMemoryAP::default();
        let mi = ADIMemoryInterface::new(0x0);
        debug_assert!(mi
            .write_block8(
                &mut mock,
                1,
                &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB] as [u8; 8])
            )
            .is_ok());
        debug_assert_eq!(
            mock.data[0..9],
            [0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA, 0xAB]
        );
    }
}
