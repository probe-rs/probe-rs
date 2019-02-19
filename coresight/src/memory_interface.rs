use super::access_port::{
    AccessPortNumber,
    AccessPortError
};
use super::access_port::consts::*;
use super::dap_access::DAPAccess;

pub enum MemoryReadSize {
    U8 = CSW_SIZE8 as isize,
    U16 = CSW_SIZE16 as isize,
    U32 = CSW_SIZE32 as isize,
}

pub trait ToMemoryReadSize {
    /// The alignment mask that is required to test for properly aligned memory.
    const ALIGNMENT_MASK: u32;
    /// The transfer size expressed as command bits for a CoreSight command.
    const MEMORY_TRANSFER_SIZE: u32;
    /// Transform a generic 32 bit sized value to a transfer size sized one.
    fn to_result(value: u32) -> Self;
    /// Transform a generic transfer size sized value to a 32 bit sized one.
    fn to_input(value: &Self) -> u32;
}

impl ToMemoryReadSize for u32 {
    const ALIGNMENT_MASK: u32 = 0x3;
    const MEMORY_TRANSFER_SIZE: u32 = CSW_SIZE32;

    fn to_result(value: u32) -> Self {
        value
    }

    fn to_input(value: &Self) -> u32 {
        *value
    }
}

impl ToMemoryReadSize for u16 {
    const ALIGNMENT_MASK: u32 = 0x1;
    const MEMORY_TRANSFER_SIZE: u32 = CSW_SIZE16;

    fn to_result(value: u32) -> Self {
        value as u16
    }

    fn to_input(value: &Self) -> u32 {
        *value as u32
    }
}

impl ToMemoryReadSize for u8 {
    const ALIGNMENT_MASK: u32 = 0x0;
    const MEMORY_TRANSFER_SIZE: u32 = CSW_SIZE8;

    fn to_result(value: u32) -> Self {
        value as u8
    }

    fn to_input(value: &Self) -> u32 {
        *value as u32
    }
}

/// A struct to give access to a targets memory using a certain DAP.
pub struct MemoryInterface {
    access_port: AccessPortNumber,
}

impl MemoryInterface {
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(access_port: AccessPortNumber) -> Self {
        Self {
            access_port
        }
    }

    /// Read a 32 bit register on the DAP.
    fn read_reg(&self, debug_port: &mut impl DAPAccess, addr: u16) -> Result<u32, AccessPortError> {
        debug_port.read_register(self.access_port, addr).or_else(|e| { println!("{:?}", e); Err(e) }).or_else(|_| Err(AccessPortError::ProbeError))
    }

    /// Write a 32 bit register on the DAP.
    fn write_reg(&self, debug_port: &mut impl DAPAccess, addr: u16, data: u32) -> Result<(), AccessPortError> {
        debug_port.write_register(self.access_port, addr, data).or_else(|_| Err(AccessPortError::ProbeError))
    }

    /// Read a word of the size defined by S at `addr`.
    /// 
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read<S: ToMemoryReadSize>(&self, debug_port: &mut impl DAPAccess, addr: u32) -> Result<S, AccessPortError> {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            self.write_reg(debug_port, MEM_AP_TAR, addr)?;
            let result = self.read_reg(debug_port, MEM_AP_DRW)?;
            Ok(S::to_result(result))
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Like `read_block` but with much simpler stucture but way lower performance for u8 and u16.
    pub fn read_block_simple<S: ToMemoryReadSize>(
        &self,
        debug_port: &mut impl DAPAccess,
        addr: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError> {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            let unit_size = std::mem::size_of::<S>() as u32;
            let len = data.len() as u32;
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            for offset in 0..len {
                let addr = addr + offset * unit_size;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                data[offset as usize] = S::to_result(self.read_reg(debug_port, MEM_AP_DRW)?);
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
    pub fn read_block<S: ToMemoryReadSize + std::fmt::LowerHex + std::fmt::Debug>(
        &self,
        debug_port: &mut impl DAPAccess,
        addr: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError> {
        // In the context of this function, a word has size S. All other sizes are given in bits.
        // One byte is 8 bits.
        if (addr & S::ALIGNMENT_MASK) == 0 {
            // Store the size of one word in bytes.
            let bytes_per_word = std::mem::size_of::<S>() as u32;
            // Calculate how many words a 32 bit value consists of.
            let f = 4 / bytes_per_word;
            // The words of size S we have to read until we can do 32 bit aligned reads.
            let num_words_at_start = (4 - (addr & 0x3)) / bytes_per_word;
            // The words of size S we have to read until we can do 32 bit aligned reads.
            let num_words_at_end = (data.len() as u32 - num_words_at_start) % f;
            // The number of 32 bit reads that are required in the second phase.
            let num_32_bit_reads = (data.len() as u32 - num_words_at_start - num_words_at_end) / f;

            // First we read data until we can do aligned 32 bit reads.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            for offset in 0..num_words_at_start {
                let addr = addr + offset * bytes_per_word;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                data[offset as usize] = S::to_result(self.read_reg(debug_port, MEM_AP_DRW)?);
            }

            // Second we read in 32 bit reads until we have less than 32 bits left to read.
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | CSW_SIZE32)?;
            for offset in 0..num_32_bit_reads {
                let addr = addr + num_words_at_start * bytes_per_word + offset * 4;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                let value = self.read_reg(debug_port, MEM_AP_DRW)?;
                for i in 0..f {
                    data[(num_words_at_start + offset * f + i) as usize] = S::to_result(value >> (i * bytes_per_word * 8));
                }
            }

            // Lastly we read data until we can have read all the remaining data that was requested.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            for offset in 0..num_words_at_end {
                let addr = addr + num_words_at_start * bytes_per_word + num_32_bit_reads * 4 + offset * bytes_per_word;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                data[(num_words_at_start + num_32_bit_reads * f + offset) as usize] = S::to_result(self.read_reg(debug_port, MEM_AP_DRW)?);
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
    pub fn write<S: ToMemoryReadSize>(
        &self,
        debug_port: &mut impl DAPAccess,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError> {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE)?;
            self.write_reg(debug_port, MEM_AP_TAR, addr)?;
            self.write_reg(debug_port, MEM_AP_DRW, S::to_input(&data))?;
            Ok(())
        } else {
            Err(AccessPortError::MemoryNotAligned)
        }
    }

    /// Like `write_block` but with much simpler stucture but way lower performance for u8 and u16.
    pub fn write_block<S: ToMemoryReadSize>(
        &self,
        debug_port: &mut impl DAPAccess,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
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
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            for offset in 0..num_words_at_start {
                let addr = addr + offset * bytes_per_word;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                self.write_reg(debug_port, MEM_AP_DRW, S::to_input(&data[offset as usize]))?;
            }

            // Second we write in 32 bit reads until we have less than 32 bits left to write.
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | CSW_SIZE32)?;
            for offset in 0..num_32_bit_writes {
                let addr = addr + num_words_at_start * bytes_per_word + offset * 4;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                for i in 0..f {
                    self.write_reg(debug_port, MEM_AP_TAR, addr + i * bytes_per_word)?;
                    self.write_reg(debug_port, MEM_AP_DRW, S::to_input(&data[(num_words_at_start + offset * f + i) as usize]))?;
                }
            }

            // Lastly we write data until we can have written all the remaining data that was requested.
            // This will at a maximum be 24 bits for 8 bit transfer size and 16 bits for 16 bit transfers.
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE as u32)?;
            for offset in 0..num_words_at_end {
                let addr = addr + num_words_at_start * bytes_per_word + num_32_bit_writes * 4 + offset * bytes_per_word;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                self.write_reg(debug_port, MEM_AP_DRW, S::to_input(&data[(num_words_at_start + num_32_bit_writes * f + offset) as usize]))?;
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
    pub fn write_block_simple<S: ToMemoryReadSize>(
        &self,
        debug_port: &mut impl DAPAccess,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
        if (addr & S::ALIGNMENT_MASK) == 0 {
            let len = data.len() as u32;
            let unit_size = std::mem::size_of::<S>() as u32;
            self.write_reg(debug_port, MEM_AP_CSW, CSW_VALUE | S::MEMORY_TRANSFER_SIZE)?;
            for offset in 0..len {
                let addr = addr + offset * unit_size;
                self.write_reg(debug_port, MEM_AP_TAR, addr)?;
                self.write_reg(debug_port, MEM_AP_DRW, S::to_input(&data[offset as usize]))?;
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
    use super::super::dap_access::MockDAP;

    #[test]
    fn read_u32() {
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xDEADBEEF as u32).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u16() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xBEEF as u16).is_ok());
        debug_assert!(mi.write(&mut mock, 2, 0xDEAD as u16).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_u8() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write(&mut mock, 0, 0xEF as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 1, 0xBE as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 2, 0xAD as u8).is_ok());
        debug_assert!(mi.write(&mut mock, 3, 0xDE as u8).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn read_block_u32() {
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u32; 4];
        debug_assert!(mi.read_block(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 3, &mut data).is_err());
    }

    #[test]
    fn read_block_u16() {
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        let mut data = [0 as u16; 4];
        debug_assert!(mi.read_block(&mut mock, 1, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 127, &mut data).is_err());
        debug_assert!(mi.read_block(&mut mock, 3, &mut data).is_err());
    }

    #[test]
    fn read_block_u8() {
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
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
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u32_only_1_word() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xDEADBEEF] as [u32; 1])).is_ok());
        debug_assert_eq!(mock.data[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn write_block_u32_unaligned_should_error() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
        debug_assert!(mi.write_block(&mut mock, 127, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
        debug_assert!(mi.write_block(&mut mock, 3, &([0xDEADBEEF, 0xABBABABE] as [u32; 2])).is_err());
    }

    #[test]
    fn write_block_u16() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u16_unaligned2() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 2, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_ok());
        debug_assert_eq!(mock.data[0..10], [0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u16_unaligned_should_error() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        debug_assert!(mi.write_block(&mut mock, 127, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
        debug_assert!(mi.write_block(&mut mock, 3, &([0xBEEF, 0xDEAD, 0xBABE, 0xABBA] as [u16; 4])).is_err());
    }

    #[test]
    fn write_block_u8() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 0, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..8], [0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u8_unaligned() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 3, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..11], [0x00, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }

    #[test]
    fn write_block_u8_unaligned2() {
        let mut mock = MockDAP::new();
        let mi = MemoryInterface::new(0x0);
        debug_assert!(mi.write_block(&mut mock, 1, &([0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB] as [u8; 8])).is_ok());
        debug_assert_eq!(mock.data[0..9], [0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0xBE, 0xBA, 0xBA ,0xAB]);
    }
}