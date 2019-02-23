use crate::access_ports::{
    APRegister,
    memory_ap::{
        MemoryAP,
        DataSize,
        CSW,
        TAR,
        DRW,
    },
    AccessPortError,
};
use crate::ap_access::APAccess;

pub trait ToMemoryReadSize: Into<u32> + Copy {
    /// The alignment mask that is required to test for properly aligned memory.
    const ALIGNMENT_MASK: u32;
    /// The transfer size expressed as command bits for a CoreSight command.
    const MEMORY_TRANSFER_SIZE: DataSize;
    /// Transform a generic 32 bit sized value to a transfer size sized one.
    fn to_result(value: u32) -> Self;
}

impl ToMemoryReadSize for u32 {
    const ALIGNMENT_MASK: u32 = 0x3;
    const MEMORY_TRANSFER_SIZE: DataSize = DataSize::U32;

    fn to_result(value: u32) -> Self {
        value
    }
}

impl ToMemoryReadSize for u16 {
    const ALIGNMENT_MASK: u32 = 0x1;
    const MEMORY_TRANSFER_SIZE: DataSize = DataSize::U16;

    fn to_result(value: u32) -> Self {
        value as u16
    }
}

impl ToMemoryReadSize for u8 {
    const ALIGNMENT_MASK: u32 = 0x0;
    const MEMORY_TRANSFER_SIZE: DataSize = DataSize::U8;

    fn to_result(value: u32) -> Self {
        value as u8
    }
}

/// A struct to give access to a targets memory using a certain DAP.
pub struct MemoryInterface {
    access_port: MemoryAP,
}

impl MemoryInterface {
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(access_port_number: u8) -> Self {
        Self {
            access_port: MemoryAP::new(access_port_number)
        }
    }

    /// Read a 32 bit register on the given AP.
    fn read_register_ap<REGISTER, AP>(&self, debug_port: &mut AP, register: REGISTER) -> Result<REGISTER, AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>
    {
        debug_port.read_register_ap(self.access_port, register)
                  .or_else(|_| Err(AccessPortError::ProbeError))
    }

    /// Write a 32 bit register on the given AP.
    fn write_register_ap<REGISTER, AP>(&self, debug_port: &mut AP, register: REGISTER) -> Result<(), AccessPortError>
    where
        REGISTER: APRegister<MemoryAP>,
        AP: APAccess<MemoryAP, REGISTER>
    {
        debug_port.write_register_ap(self.access_port, register).or_else(|_| Err(AccessPortError::ProbeError))
    }

    /// Read a word of the size defined by S at `addr`.
    /// 
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read<S, AP>( &self, debug_port: &mut AP, address: u32) -> Result<S, AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        if (address & S::ALIGNMENT_MASK) == 0 {
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            let tar = TAR { address };
            self.write_register_ap(debug_port, csw)?;
            self.write_register_ap(debug_port, tar)?;
            let result = self.read_register_ap(debug_port, DRW::default())?;

            Ok(S::to_result(result.into()))
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Like `read_block` but with much simpler stucture but way lower performance for u8 and u16.
    pub fn read_block_simple<S, AP>(
        &self,
        debug_port: &mut AP,
        addr: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            let drw: DRW = Default::default();

            let unit_size = std::mem::size_of::<S>() as u32;
            let len = data.len() as u32;
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..len {
                let addr = addr + offset * unit_size;

                let tar = TAR { address: addr };
                self.write_register_ap(debug_port, tar)?;
                data[offset as usize] = S::to_result(self.read_register_ap(debug_port, drw)?.data);
            }
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Read a block of words of the size defined by S at `addr`.
    /// 
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read_block<S, AP>(
        &self,
        debug_port: &mut AP,
        address: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        // In the context of this function, a word has size S. All other sizes are given in bits.
        // One byte is 8 bits.
        if (address & S::ALIGNMENT_MASK) == 0 {
            // Store the size of one word in bytes.
            let bytes_per_word = std::mem::size_of::<S>() as u32;
            // Calculate how many words a 32 bit value consists of.
            let f = 4 / bytes_per_word;
            // The words of size S we have to read until we can do 32 bit aligned reads.
            let num_words_at_start = (4 - (address & 0x3)) / bytes_per_word;
            // The words of size S we have to read until we can do 32 bit aligned reads.
            let num_words_at_end = (data.len() as u32 - num_words_at_start) % f;
            // The number of 32 bit reads that are required in the second phase.
            let num_32_bit_reads = (data.len() as u32 - num_words_at_start - num_words_at_end) / f;

            // First we read data until we can do aligned 32 bit reads.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_words_at_start {
                let tar = TAR { address: address + offset * bytes_per_word };
                self.write_register_ap(debug_port, tar)?;
                data[offset as usize] = S::to_result(self.read_register_ap(debug_port, DRW::default())?.data);
            }

            // Second we read in 32 bit reads until we have less than 32 bits left to read.
            let csw: CSW = CSW { AddrInc: 1, SIZE: DataSize::U32, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_32_bit_reads {
                let tar = TAR { address: address + num_words_at_start * bytes_per_word + offset * 4 };
                self.write_register_ap(debug_port, tar)?;
                let value = self.read_register_ap(debug_port, DRW::default())?.data;
                for i in 0..f {
                    data[(num_words_at_start + offset * f + i) as usize] = S::to_result(value >> (i * bytes_per_word * 8));
                }
            }

            // Lastly we read data until we can have read all the remaining data that was requested.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_words_at_end {
                let tar = TAR { address: address + num_words_at_start * bytes_per_word + num_32_bit_reads * 4 + offset * bytes_per_word };
                self.write_register_ap(debug_port, tar)?;
                data[(num_words_at_start + num_32_bit_reads * f + offset) as usize]
                    = S::to_result(self.read_register_ap(debug_port, DRW::default())?.data);
            }
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Write a word of the size defined by S at `addr`.
    /// 
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write<S, AP>(
        &self,
        debug_port: &mut AP,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            let drw = DRW { data: data.into() };
            let tar = TAR { address: addr };
            self.write_register_ap(debug_port, csw)?;
            self.write_register_ap(debug_port, tar)?;
            self.write_register_ap(debug_port, drw)?;
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Like `write_block` but with much simpler stucture but way lower performance for u8 and u16.
    pub fn write_block<S, AP>(
        &self,
        debug_port: &mut AP,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        // In the context of this function, a word has size S. All other sizes are given in bits.
        // One byte is 8 bits.
        if (addr & S::ALIGNMENT_MASK) == 0 {
            // Store the size of one word in bytes.
            let bytes_per_word = std::mem::size_of::<S>() as u32;
            // Calculate how many words a 32 bit value consists of.
            let f = 4 / bytes_per_word;
            // The words of size S we have to write until we can do 32 bit aligned writes.
            let num_words_at_start = (4 - (addr & 0x3)) / bytes_per_word;
            // The words of size S we have to write until we can do 32 bit aligned writes.
            let num_words_at_end = (data.len() as u32 - num_words_at_start) % f;
            // The number of 32 bit writes that are required in the second phase.
            let num_32_bit_writes = (data.len() as u32 - num_words_at_start - num_words_at_end) / f;

            // First we write data until we can do aligned 32 bit writes.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_words_at_start {
                let tar = TAR { address: addr + offset * bytes_per_word };
                self.write_register_ap(debug_port, tar)?;
                let drw = DRW { data: data[offset as usize].into() };
                self.write_register_ap(debug_port, drw)?;
            }

            // Second we write in 32 bit reads until we have less than 32 bits left to write.
            let csw: CSW = CSW { AddrInc: 1, SIZE: DataSize::U32, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_32_bit_writes {
                let address = addr + num_words_at_start * bytes_per_word + offset * 4;
                for i in 0..f {
                    let tar = TAR { address: address + i * bytes_per_word };
                    self.write_register_ap(debug_port, tar)?;
                    let drw = DRW { data: data[(num_words_at_start + offset * f + i) as usize].into() };
                    self.write_register_ap(debug_port, drw)?;
                }
            }

            // Lastly we write data until we can have written all the remaining data that was requested.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..num_words_at_end {
                let tar = TAR { address: addr + num_words_at_start * bytes_per_word + num_32_bit_writes * 4 + offset * bytes_per_word };
                self.write_register_ap(debug_port, tar)?;
                let drw = DRW { data: data[(num_words_at_start + num_32_bit_writes * f + offset) as usize].into() };
                self.write_register_ap(debug_port, drw)?;
            }
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Write a block of words of the size defined by S at `addr`.
    /// 
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_block_simple<S, AP>(
        &self,
        debug_port: &mut AP,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError>
    where
        S: ToMemoryReadSize,
        AP: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
    {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            let len = data.len() as u32;
            let unit_size = std::mem::size_of::<S>() as u32;
            let csw: CSW = CSW { AddrInc: 1, SIZE: S::MEMORY_TRANSFER_SIZE, ..Default::default() };
            self.write_register_ap(debug_port, csw)?;
            for offset in 0..len {
                let tar = TAR { address: addr + offset * unit_size };
                self.write_register_ap(debug_port, tar)?;
                let drw = DRW { data: data[offset as usize].into() };
                self.write_register_ap(debug_port, drw)?;
            }
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryInterface;
    use crate::access_ports::memory_ap::mock::MockMemoryAP;

    #[test]
    fn read_u32() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mi = MemoryInterface::new(0x0);
        let read: Result<u32, _> = mi.read(&mut mock, 0);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xDEADBEEF);
    }

    #[test]
    fn read_u16() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mi = MemoryInterface::new(0x0);
        let read: Result<u16, _> = mi.read(&mut mock, 0);
        let read2: Result<u16, _> = mi.read(&mut mock, 2);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xBEEF);
        debug_assert_eq!(read2.unwrap(), 0xDEAD);
    }

    #[test]
    fn read_u8() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mi = MemoryInterface::new(0x0);
        let read: Result<u8, _> = mi.read(&mut mock, 0);
        let read2: Result<u8, _> = mi.read(&mut mock, 1);
        let read3: Result<u8, _> = mi.read(&mut mock, 2);
        let read4: Result<u8, _> = mi.read(&mut mock, 3);
        debug_assert!(read.is_ok());
        debug_assert_eq!(read.unwrap(), 0xEF);
        debug_assert_eq!(read2.unwrap(), 0xBE);
        debug_assert_eq!(read3.unwrap(), 0xAD);
        debug_assert_eq!(read4.unwrap(), 0xDE);
    }

    #[test]
    fn write_u32() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xDEADBEEF as u32).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u16() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xBEEF as u16).is_ok());
        debug_assert!(mi.write(&mut mock, 2, 0xDEAD as u16).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u8() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xEF as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 1, 0xBE as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 2, 0xAD as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 3, 0xDE as u8).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn read_block_u32() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u32; 2];
        let read = mi.read_block(&mut mock, 0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xDEADBEEF, 0xABBABABE]);
    }

    #[test]
    fn read_block_u32_only_1_word() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u32; 1];
        let read = mi.read_block(&mut mock, 0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xDEADBEEF]);
    }

    #[test]
    fn read_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u32; 4];
        debug_assert!(mi.read_block(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 3, &mut data).is_err());
    }

    #[test]
    fn read_block_u16() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block(&mut mock, 0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xBEEF, 0xDEAD, 0xBABE, 0xABBA]);
    }

    #[test]
    fn read_block_u16_unaligned() {
        let mut mock = MockMemoryAP::new();
        mock.data[2] = 0xEF;
        mock.data[3] = 0xBE;
        mock.data[4] = 0xAD;
        mock.data[5] = 0xDE;
        mock.data[6] = 0xBE;
        mock.data[7] = 0xBA;
        mock.data[8] = 0xBA;
        mock.data[9] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        let read = mi.read_block(&mut mock, 2, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xBEEF, 0xDEAD, 0xBABE, 0xABBA]);
    }

    #[test]
    fn read_block_u16_unaligned_should_error() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        debug_assert!(mi.read_block(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 3, &mut data).is_err());
    }

    #[test]
    fn read_block_u8() {
        let mut mock = MockMemoryAP::new();
        mock.data[0] = 0xEF;
        mock.data[1] = 0xBE;
        mock.data[2] = 0xAD;
        mock.data[3] = 0xDE;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xBA;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block(&mut mock, 0, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn read_block_u8_unaligned() {
        let mut mock = MockMemoryAP::new();
        mock.data[1] = 0xEF;
        mock.data[2] = 0xBE;
        mock.data[3] = 0xAD;
        mock.data[4] = 0xDE;
        mock.data[5] = 0xBE;
        mock.data[6] = 0xBA;
        mock.data[7] = 0xBA;
        mock.data[8] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block(&mut mock, 1, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn read_block_u8_unaligned2() {
        let mut mock = MockMemoryAP::new();
        mock.data[3] = 0xEF;
        mock.data[4] = 0xBE;
        mock.data[5] = 0xAD;
        mock.data[6] = 0xDE;
        mock.data[7] = 0xBE;
        mock.data[8] = 0xBA;
        mock.data[9] = 0xBA;
        mock.data[10] = 0xAB;
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u8; 8];
        let read = mi.read_block(&mut mock, 3, &mut data);
        debug_assert!(read.is_ok());
        debug_assert_eq!(data, [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u32() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u32_only_1_word() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xDEADBEEF] as [u32; 1])).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
        debug_assert!(mi.write_block(&mut mock, 127, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
        debug_assert!(mi.write_block(&mut mock, 3, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
    }

    #[test]
    fn write_block_u16() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u16_unaligned2() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 2, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        debug_assert_eq!(mock.data[0..10], [0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u16_unaligned_should_error() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        debug_assert!(mi.write_block(&mut mock, 127, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        debug_assert!(mi.write_block(&mut mock, 3, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
    }

    #[test]
    fn write_block_u8() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u8_unaligned() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 3, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..11], [0x00, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u8_unaligned2() {
        let mut mock = MockMemoryAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..9], [0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }
}