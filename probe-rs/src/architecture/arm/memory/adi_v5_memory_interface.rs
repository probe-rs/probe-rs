use super::super::ap::{
    AccessPortError, AddressIncrement, ApAccess, ApRegister, DataSize, MemoryAp, CSW, DRW, TAR,
};
use crate::architecture::arm::{dp::DpAccess, MemoryApInformation};
use crate::{CommunicationInterface, CoreRegister, CoreRegisterAddress, DebugProbeError, Error};
use scroll::{Pread, Pwrite, LE};
use std::convert::TryInto;
use std::{
    ops::Range,
    time::{Duration, Instant},
};

use bitfield::bitfield;

pub trait ArmProbe {
    fn read_core_reg(&mut self, ap: MemoryAp, addr: CoreRegisterAddress) -> Result<u32, Error>;
    fn write_core_reg(
        &mut self,
        ap: MemoryAp,
        addr: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), Error>;

    fn read_8(&mut self, ap: MemoryAp, address: u32, data: &mut [u8]) -> Result<(), Error>;
    fn read_32(&mut self, ap: MemoryAp, address: u32, data: &mut [u32]) -> Result<(), Error>;

    fn write_8(&mut self, ap: MemoryAp, address: u32, data: &[u8]) -> Result<(), Error>;
    fn write_32(&mut self, ap: MemoryAp, address: u32, data: &[u32]) -> Result<(), Error>;

    fn flush(&mut self) -> Result<(), Error>;
}

/// A struct to give access to a targets memory using a certain DAP.
pub(crate) struct ADIMemoryInterface<'interface, AP>
where
    AP: CommunicationInterface + ApAccess + DpAccess,
{
    interface: &'interface mut AP,
    only_32bit_data_size: bool,

    // Does the connected memory AP support the HNONSEC bit?
    // If it doesn't support it, bit 30 in the CSW register has
    // to be set to 1 at all times.
    supports_hnonsec: bool,

    // Cached value of the CSW register, to avoid unecessary writes.
    //
    // TODO: This is the wrong location for this, it should actually be
    // cached on a lower level, where the other Memory AP information is
    // stored.
    cached_csw_value: Option<CSW>,
}

impl<'interface, AP> ADIMemoryInterface<'interface, AP>
where
    AP: CommunicationInterface + ApAccess + DpAccess,
{
    /// Creates a new MemoryInterface for given AccessPort.
    pub fn new(
        interface: &'interface mut AP,
        ap_information: &MemoryApInformation,
    ) -> Result<ADIMemoryInterface<'interface, AP>, AccessPortError> {
        Ok(Self {
            interface,
            only_32bit_data_size: ap_information.only_32bit_data_size,
            supports_hnonsec: ap_information.supports_hnonsec,
            cached_csw_value: None,
        })
    }
}

impl<AP> ADIMemoryInterface<'_, AP>
where
    AP: CommunicationInterface + ApAccess + DpAccess,
{
    /// Build the correct CSW register for a memory access
    ///
    /// Currently, only AMBA AHB Access is supported.
    fn build_csw_register(&self, data_size: DataSize) -> CSW {
        // The CSW Register is set for an AMBA AHB Acccess, according to
        // the ARM Debug Interface Architecture Specification.
        //
        // The HNONSEC bit is set according to [Self::supports_hnonsec]:
        //
        //  HNONSEC[30]          = 1  - Should be One, if unsupported, otherwise
        //                              zero to indicate secure access
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
            HNONSEC: if self.supports_hnonsec { 0 } else { 1 },
            PROT: 0b10,
            CACHE: 0b11,
            AddrInc: AddressIncrement::Single,
            SIZE: data_size,
            ..Default::default()
        }
    }

    fn write_csw_register(
        &mut self,
        access_port: MemoryAp,
        value: CSW,
    ) -> Result<(), AccessPortError> {
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

    fn wait_for_core_register_transfer(
        &mut self,
        access_port: MemoryAp,
        timeout: Duration,
    ) -> Result<(), Error> {
        // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
        // (see C1-292, cortex m0 arm)
        let start = Instant::now();

        while start.elapsed() < timeout {
            let dhcsr_val = Dhcsr(self.read_word_32(access_port, Dhcsr::ADDRESS)?);

            if dhcsr_val.s_regrdy() {
                return Ok(());
            }
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }

    /// Read a 32 bit register on the given AP.
    fn read_ap_register<R>(&mut self, access_port: MemoryAp) -> Result<R, AccessPortError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .read_ap_register(access_port)
            .map_err(AccessPortError::register_read_error::<R, _>)
    }

    /// Read multiple 32 bit values from the same
    /// register on the given AP.
    fn read_ap_register_repeated<R>(
        &mut self,
        access_port: MemoryAp,
        register: R,
        values: &mut [u32],
    ) -> Result<(), AccessPortError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .read_ap_register_repeated(access_port, register, values)
            .map_err(AccessPortError::register_read_error::<R, _>)
    }

    /// Write a 32 bit register on the given AP.
    fn write_ap_register<R>(
        &mut self,
        access_port: MemoryAp,
        register: R,
    ) -> Result<(), AccessPortError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .write_ap_register(access_port, register)
            .map_err(AccessPortError::register_write_error::<R, _>)
    }

    /// Write multiple 32 bit values to the same
    /// register on the given AP.
    fn write_ap_register_repeated<R>(
        &mut self,
        access_port: MemoryAp,
        register: R,
        values: &[u32],
    ) -> Result<(), AccessPortError>
    where
        R: ApRegister<MemoryAp>,
        AP: ApAccess,
    {
        self.interface
            .write_ap_register_repeated(access_port, register, values)
            .map_err(AccessPortError::register_write_error::<R, _>)
    }

    /// Read a 32bit word at `addr`.
    ///
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read_word_32(
        &mut self,
        access_port: MemoryAp,
        address: u32,
    ) -> Result<u32, AccessPortError> {
        if (address % 4) != 0 {
            return Err(AccessPortError::alignment_error(address, 4));
        }

        let csw = self.build_csw_register(DataSize::U32);

        let tar = TAR { address };

        self.write_csw_register(access_port, csw)?;
        self.write_ap_register(access_port, tar)?;
        let result: DRW = self.read_ap_register(access_port)?;

        Ok(result.data)
    }

    /// Read an 8bit word at `addr`.
    pub fn read_word_8(
        &mut self,
        access_port: MemoryAp,
        address: u32,
    ) -> Result<u8, AccessPortError> {
        let aligned = aligned_range(address, 1)?;

        // Offset of byte in word (little endian)
        let bit_offset = (address - aligned.start) * 8;

        let result = if self.only_32bit_data_size {
            // Read 32-bit word and extract the correct byte
            ((self.read_word_32(access_port, aligned.start)? >> bit_offset) & 0xFF) as u8
        } else {
            let csw = self.build_csw_register(DataSize::U8);
            let tar = TAR { address };
            self.write_csw_register(access_port, csw)?;
            self.write_ap_register(access_port, tar)?;
            let result: DRW = self.read_ap_register(access_port)?;

            // Extract the correct byte
            // See "Arm Debug Interface Architecture Specification ADIv5.0 to ADIv5.2", C2.2.6
            ((result.data >> bit_offset) & 0xFF) as u8
        };

        Ok(result)
    }

    /// Read a block of words of the size defined by S at `addr`.
    ///
    /// The number of words read is `data.len()`.
    /// The address where the read should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn read_32(
        &mut self,
        access_port: MemoryAp,
        start_address: u32,
        data: &mut [u32],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        if (start_address % 4) != 0 {
            return Err(AccessPortError::alignment_error(start_address, 4));
        }

        // Second we read in 32 bit reads until we have less than 32 bits left to read.
        let csw = self.build_csw_register(DataSize::U32);
        self.write_csw_register(access_port, csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(access_port, tar)?;

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
            access_port,
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
            self.write_ap_register(access_port, tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Reading chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.read_ap_register_repeated(
                access_port,
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

    pub fn read_8(
        &mut self,
        access_port: MemoryAp,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        let aligned = aligned_range(address, data.len())?;

        // Read aligned block of 32-bit words
        let mut buf32 = vec![0u32; aligned.len() / 4];
        self.read_32(access_port, aligned.start, &mut buf32)?;

        // Convert 32-bit words to bytes
        let mut buf8 = vec![0u8; aligned.len()];
        for (i, word) in buf32.into_iter().enumerate() {
            buf8.pwrite_with(word, i * 4, LE).unwrap();
        }

        // Copy relevant part of aligned block to output data
        let start = (address - aligned.start) as usize;
        data.copy_from_slice(&buf8[start..start + data.len()]);

        Ok(())
    }

    /// Write a 32bit word at `addr`.
    ///
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_word_32(
        &mut self,
        access_port: MemoryAp,
        address: u32,
        data: u32,
    ) -> Result<(), AccessPortError> {
        if (address % 4) != 0 {
            return Err(AccessPortError::alignment_error(address, 4));
        }

        let csw = self.build_csw_register(DataSize::U32);
        let drw = DRW { data };
        let tar = TAR { address };
        self.write_csw_register(access_port, csw)?;

        self.write_ap_register(access_port, tar)?;
        self.write_ap_register(access_port, drw)?;

        Ok(())
    }

    /// Write an 8bit word at `addr`.
    pub fn write_word_8(
        &mut self,
        access_port: MemoryAp,
        address: u32,
        data: u8,
    ) -> Result<(), AccessPortError> {
        let aligned = aligned_range(address, 1)?;

        // Offset of byte in word (little endian)
        let bit_offset = (address - aligned.start) * 8;

        if self.only_32bit_data_size {
            // Read the existing 32-bit word and insert the byte at the correct bit offset
            // See "Arm Debug Interface Architecture Specification ADIv5.0 to ADIv5.2", C2.2.6
            let word = self.read_word_32(access_port, aligned.start)?;
            let word = word & !(0xFF << bit_offset) | (u32::from(data) << bit_offset);

            self.write_word_32(access_port, aligned.start, word)?;
        } else {
            let csw = self.build_csw_register(DataSize::U8);
            let drw = DRW {
                data: u32::from(data) << bit_offset,
            };
            let tar = TAR { address };
            self.write_csw_register(access_port, csw)?;

            self.write_ap_register(access_port, tar)?;
            self.write_ap_register(access_port, drw)?;
        }

        Ok(())
    }

    /// Write a block of 32bit words at `addr`.
    ///
    /// The number of words written is `data.len()`.
    /// The address where the write should be performed at has to be word aligned.
    /// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
    pub fn write_32(
        &mut self,
        access_port: MemoryAp,
        start_address: u32,
        data: &[u32],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        if (start_address % 4) != 0 {
            return Err(AccessPortError::alignment_error(start_address, 4));
        }

        log::debug!(
            "Write block with total size {} bytes to address {:#08x}",
            data.len() * 4,
            start_address
        );

        // Second we write in 32 bit reads until we have less than 32 bits left to write.
        let csw = self.build_csw_register(DataSize::U32);

        self.write_csw_register(access_port, csw)?;

        let mut address = start_address;
        let tar = TAR { address };
        self.write_ap_register(access_port, tar)?;

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
            access_port,
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
            self.write_ap_register(access_port, tar)?;

            let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

            log::debug!(
                "Writing chunk with len {} at address {:#08x}",
                next_chunk_size_bytes,
                address
            );

            let next_chunk_size_words = next_chunk_size_bytes / 4;

            self.write_ap_register_repeated(
                access_port,
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
    pub fn write_8(
        &mut self,
        access_port: MemoryAp,
        address: u32,
        data: &[u8],
    ) -> Result<(), AccessPortError> {
        if data.is_empty() {
            return Ok(());
        }

        let aligned = aligned_range(address, data.len())?;

        // Create buffer with aligned size
        let mut buf8 = vec![0u8; aligned.len()];

        // If the start of the range isn't aligned, read the first word in to avoid clobbering
        if address != aligned.start {
            buf8.pwrite_with(self.read_word_32(access_port, aligned.start)?, 0, LE)
                .unwrap();
        }

        // If the end of the range isn't aligned, read the last word in to avoid clobbering
        if address + data.len() as u32 != aligned.end {
            buf8.pwrite_with(
                self.read_word_32(access_port, aligned.end - 4)?,
                aligned.len() - 4,
                LE,
            )
            .unwrap();
        }

        // Copy input data into buffer at the correct location
        let start = (address - aligned.start) as usize;
        buf8[start..start + data.len()].copy_from_slice(&data);

        // Convert buffer to 32-bit words
        let mut buf32 = vec![0u32; aligned.len() / 4];
        for (i, word) in buf32.iter_mut().enumerate() {
            *word = buf8.pread_with(i * 4, LE).unwrap();
        }

        // Write aligned block into memory
        self.write_32(access_port, aligned.start, &buf32)?;

        Ok(())
    }
}

impl<AP> ArmProbe for ADIMemoryInterface<'_, AP>
where
    AP: CommunicationInterface + ApAccess + DpAccess,
{
    fn read_core_reg(&mut self, ap: MemoryAp, addr: CoreRegisterAddress) -> Result<u32, Error> {
        // Write the DCRSR value to select the register we want to read.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(false); // Perform a read.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to read.

        self.write_word_32(ap, Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(ap, Duration::from_millis(100))?;

        let value = self.read_word_32(ap, Dcrdr::ADDRESS)?;

        Ok(value)
    }

    fn write_core_reg(
        &mut self,
        ap: MemoryAp,
        addr: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), Error> {
        self.write_word_32(ap, Dcrdr::ADDRESS, value)?;

        // write the DCRSR value to select the register we want to write.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(true); // Perform a write.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

        self.write_word_32(ap, Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(ap, Duration::from_millis(100))?;

        Ok(())
    }

    fn read_8(&mut self, ap: MemoryAp, address: u32, data: &mut [u8]) -> Result<(), Error> {
        if data.len() == 1 {
            data[0] = self.read_word_8(ap, address)?;
        } else {
            self.read_8(ap, address, data)?;
        }

        Ok(())
    }

    fn read_32(&mut self, ap: MemoryAp, address: u32, data: &mut [u32]) -> Result<(), Error> {
        if data.len() == 1 {
            data[0] = self.read_word_32(ap, address)?;
        } else {
            self.read_32(ap, address, data)?;
        }

        Ok(())
    }

    fn write_8(&mut self, ap: MemoryAp, address: u32, data: &[u8]) -> Result<(), Error> {
        if data.len() == 1 {
            self.write_word_8(ap, address, data[0])?;
        } else {
            self.write_8(ap, address, data)?;
        }

        Ok(())
    }

    fn write_32(&mut self, ap: MemoryAp, address: u32, data: &[u32]) -> Result<(), Error> {
        if data.len() == 1 {
            self.write_word_32(ap, address, data[0])?;
        } else {
            self.write_32(ap, address, data)?;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.interface.flush()?;

        Ok(())
    }
}

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Dhcsr(u32);
    impl Debug;
    pub s_reset_st, _: 25;
    pub s_retire_st, _: 24;
    pub s_lockup, _: 19;
    pub s_sleep, _: 18;
    pub s_halt, _: 17;
    pub s_regrdy, _: 16;
    pub c_maskints, set_c_maskints: 3;
    pub c_step, set_c_step: 2;
    pub c_halt, set_c_halt: 1;
    pub c_debugen, set_c_debugen: 0;
}

impl Dhcsr {
    /// This function sets the bit to enable writes to this register.
    ///
    /// C1.6.3 Debug Halting Control and Status Register, DHCSR:
    /// Debug key:
    /// Software must write 0xA05F to this field to enable write accesses to bits
    /// [15:0], otherwise the processor ignores the write access.
    pub fn enable_write(&mut self) {
        self.0 &= !(0xffff << 16);
        self.0 |= 0xa05f << 16;
    }
}

impl From<u32> for Dhcsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dhcsr> for u32 {
    fn from(value: Dhcsr) -> Self {
        value.0
    }
}

impl CoreRegister for Dhcsr {
    const ADDRESS: u32 = 0xE000_EDF0;
    const NAME: &'static str = "DHCSR";
}

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Dcrsr(u32);
    impl Debug;
    pub _, set_regwnr: 16;
    pub _, set_regsel: 4,0;
}

impl From<u32> for Dcrsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dcrsr> for u32 {
    fn from(value: Dcrsr) -> Self {
        value.0
    }
}

impl CoreRegister for Dcrsr {
    const ADDRESS: u32 = 0xE000_EDF4;
    const NAME: &'static str = "DCRSR";
}

#[derive(Debug, Copy, Clone)]
pub struct Dcrdr(u32);

impl From<u32> for Dcrdr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dcrdr> for u32 {
    fn from(value: Dcrdr) -> Self {
        value.0
    }
}

impl CoreRegister for Dcrdr {
    const ADDRESS: u32 = 0xE000_EDF8;
    const NAME: &'static str = "DCRDR";
}

/// Calculates a 32-bit word aligned range from an address/length pair.
fn aligned_range(address: u32, len: usize) -> Result<Range<u32>, AccessPortError> {
    // Round start address down to the nearest multiple of 4
    let start = address - (address % 4);

    let unaligned_end = len
        .try_into()
        .ok()
        .and_then(|len: u32| len.checked_add(address))
        .ok_or(AccessPortError::OutOfBoundsError)?;

    // Round end address up to the nearest multiple of 4
    let end = unaligned_end
        .checked_add((4 - (unaligned_end % 4)) % 4)
        .ok_or(AccessPortError::OutOfBoundsError)?;

    Ok(Range { start, end })
}

#[cfg(test)]
mod tests {
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
                only_32bit_data_size: false,
                debug_base_address: 0xf000_0000,
                supports_hnonsec: false,
            };
            Self::new(mock, &ap_information).unwrap()
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

            mi.write_word_8(DUMMY_AP, address as u32, DATA8[0])
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
