use super::super::ap::{
    APAccess, APRegister, AccessPortError, AddressIncrement, DataSize, MemoryAP, CSW, DRW, TAR,
};
use crate::CommunicationInterface;
use scroll::{Pread, Pwrite, LE};
use std::convert::TryInto;
use std::ops::Range;

/// Build the correct CSW register for a memory access
///
/// Currently, only AMBA AHB Access is supported.
pub fn build_csw_register(data_size: DataSize) -> CSW {
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
fn read_ap_register<AP, R>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    register: R,
) -> Result<R, AccessPortError>
where
    R: APRegister<MemoryAP>,
    AP: APAccess<MemoryAP, R>,
{
    interface
        .read_ap_register(access_port, register)
        .map_err(AccessPortError::register_read_error::<R, _>)
}

/// Read multiple 32 bit values from the same
/// register on the given AP.
fn read_ap_register_repeated<AP, R>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    register: R,
    values: &mut [u32],
) -> Result<(), AccessPortError>
where
    R: APRegister<MemoryAP>,
    AP: APAccess<MemoryAP, R>,
{
    interface
        .read_ap_register_repeated(access_port, register, values)
        .map_err(AccessPortError::register_read_error::<R, _>)
}

/// Write a 32 bit register on the given AP.
fn write_ap_register<AP, R>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    register: R,
) -> Result<(), AccessPortError>
where
    R: APRegister<MemoryAP>,
    AP: APAccess<MemoryAP, R>,
{
    interface
        .write_ap_register(access_port, register)
        .map_err(AccessPortError::register_write_error::<R, _>)
}

/// Write multiple 32 bit values to the same
/// register on the given AP.
fn write_ap_register_repeated<AP, R>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    register: R,
    values: &[u32],
) -> Result<(), AccessPortError>
where
    R: APRegister<MemoryAP>,
    AP: APAccess<MemoryAP, R>,
{
    interface
        .write_ap_register_repeated(access_port, register, values)
        .map_err(AccessPortError::register_write_error::<R, _>)
}

/// Read a 32bit word at `addr`.
///
/// The address where the read should be performed at has to be word aligned.
/// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
pub fn read_word_32<AP>(
    interface: &mut AP,
    access_port_number: impl Into<MemoryAP> + Copy,
    address: u32,
) -> Result<u32, AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    if (address % 4) != 0 {
        return Err(AccessPortError::alignment_error(address, 4));
    }

    let csw = build_csw_register(DataSize::U32);

    let tar = TAR { address };
    write_ap_register(interface, access_port_number, csw)?;
    write_ap_register(interface, access_port_number, tar)?;
    let result = read_ap_register(interface, access_port_number, DRW::default())?;

    Ok(result.data)
}

/// Read an 8bit word at `addr`.
pub fn read_word_8<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
) -> Result<u8, AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    let aligned = aligned_range(address, 1)?;

    // Offset of byte in word (little endian)
    let bit_offset = (address - aligned.start) * 8;

    // Read 32-bit word and extract the correct byte
    Ok(((read_word_32(interface, access_port, aligned.start)? >> bit_offset) & 0xFF) as u8)
}

/// Read an 8bit word at `addr`.
pub fn read_word_8_true<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
) -> Result<u8, AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    let aligned = aligned_range(address, 1)?;

    // Offset of byte in word (little endian)
    let bit_offset = (address - aligned.start) * 8;

    let csw = build_csw_register(DataSize::U8);
    let tar = TAR { address };
    write_ap_register(interface, access_port, csw)?;
    write_ap_register(interface, access_port, tar)?;
    let result = read_ap_register(interface, access_port, DRW::default())?;

    // Extract the correct byte
    // See "Arm Debug Interface Architecture Specification ADIv5.0 to ADIv5.2", C2.2.6
    Ok(((result.data >> bit_offset) & 0xFF) as u8)
}

/// Read a block of words of the size defined by S at `addr`.
///
/// The number of words read is `data.len()`.
/// The address where the read should be performed at has to be word aligned.
/// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
pub fn read_32<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    start_address: u32,
    data: &mut [u32],
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    if data.is_empty() {
        return Ok(());
    }

    if (start_address % 4) != 0 {
        return Err(AccessPortError::alignment_error(start_address, 4));
    }

    // Second we read in 32 bit reads until we have less than 32 bits left to read.
    let csw = build_csw_register(DataSize::U32);
    write_ap_register(interface, access_port, csw)?;

    let mut address = start_address;
    let tar = TAR { address };
    write_ap_register(interface, access_port, tar)?;

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

    read_ap_register_repeated(
        interface,
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
        write_ap_register(interface, access_port, tar)?;

        let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

        log::debug!(
            "Reading chunk with len {} at address {:#08x}",
            next_chunk_size_bytes,
            address
        );

        let next_chunk_size_words = next_chunk_size_bytes / 4;

        read_ap_register_repeated(
            interface,
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

pub fn read_8<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
    data: &mut [u8],
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    if data.is_empty() {
        return Ok(());
    }

    let aligned = aligned_range(address, data.len())?;

    // Read aligned block of 32-bit words
    let mut buf32 = vec![0u32; aligned.len() / 4];
    read_32(interface, access_port, aligned.start, &mut buf32)?;

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
pub fn write_word_32<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
    data: u32,
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    if (address % 4) != 0 {
        return Err(AccessPortError::alignment_error(address, 4));
    }

    let csw = build_csw_register(DataSize::U32);
    let drw = DRW { data };
    let tar = TAR { address };
    write_ap_register(interface, access_port, csw)?;
    write_ap_register(interface, access_port, tar)?;
    write_ap_register(interface, access_port, drw)?;

    // Ensure the write is actually performed.
    let _ = write_ap_register(interface, access_port, csw);

    Ok(())
}

/// Write an 8bit word at `addr`.
pub fn write_word_8<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
    data: u8,
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    let aligned = aligned_range(address, 1)?;

    // Offset of byte in word (little endian)
    let bit_offset = (address - aligned.start) * 8;

    // Read the existing 32-bit word and insert the byte at the correct bit offset
    // See "Arm Debug Interface Architecture Specification ADIv5.0 to ADIv5.2", C2.2.6
    let word = read_word_32(interface, access_port, aligned.start)?;
    let word = word & !(0xFF << bit_offset) | (u32::from(data) << bit_offset);

    write_word_32(interface, access_port, aligned.start, word)?;

    Ok(())
}

/// Write an 8bit word at `addr`.
pub fn write_word_8_true<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
    data: u8,
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    let aligned = aligned_range(address, 1)?;

    // Offset of byte in word (little endian)
    let bit_offset = (address - aligned.start) * 8;

    let csw = build_csw_register(DataSize::U8);
    let drw = DRW {
        data: u32::from(data) << bit_offset,
    };
    let tar = TAR { address };
    write_ap_register(interface, access_port, csw)?;
    write_ap_register(interface, access_port, tar)?;
    write_ap_register(interface, access_port, drw)?;

    Ok(())
}

/// Write a block of 32bit words at `addr`.
///
/// The number of words written is `data.len()`.
/// The address where the write should be performed at has to be word aligned.
/// Returns `AccessPortError::MemoryNotAligned` if this does not hold true.
pub fn write_32<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    start_address: u32,
    data: &[u32],
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
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
    let csw = build_csw_register(DataSize::U32);

    write_ap_register(interface, access_port, csw)?;

    let mut address = start_address;
    let tar = TAR { address };
    write_ap_register(interface, access_port, tar)?;

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

    write_ap_register_repeated(
        interface,
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
        write_ap_register(interface, access_port, tar)?;

        let next_chunk_size_bytes = std::cmp::min(max_chunk_size_bytes, remaining_data_len * 4);

        log::debug!(
            "Writing chunk with len {} at address {:#08x}",
            next_chunk_size_bytes,
            address
        );

        let next_chunk_size_words = next_chunk_size_bytes / 4;

        write_ap_register_repeated(
            interface,
            access_port,
            DRW { data: 0 },
            &data[data_offset..(data_offset + next_chunk_size_words)],
        )?;

        remaining_data_len -= next_chunk_size_words;
        address += (4 * next_chunk_size_words) as u32;
        data_offset += next_chunk_size_words;
    }

    // Ensure the last write is actually performed
    write_ap_register(interface, access_port, csw)?;

    log::debug!("Finished writing block");

    Ok(())
}

/// Write a block of 8bit words at `addr`.
///
/// The number of words written is `data.len()`.
pub fn write_8<AP>(
    interface: &mut AP,
    access_port: impl Into<MemoryAP> + Copy,
    address: u32,
    data: &[u8],
) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    if data.is_empty() {
        return Ok(());
    }

    let aligned = aligned_range(address, data.len())?;

    // Create buffer with aligned size
    let mut buf8 = vec![0u8; aligned.len()];

    // If the start of the range isn't aligned, read the first word in to avoid clobbering
    if address != aligned.start {
        buf8.pwrite_with(read_word_32(interface, access_port, aligned.start)?, 0, LE)
            .unwrap();
    }

    // If the end of the range isn't aligned, read the last word in to avoid clobbering
    if address + data.len() as u32 != aligned.end {
        buf8.pwrite_with(
            read_word_32(interface, access_port, aligned.end - 4)?,
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
    write_32(interface, access_port, aligned.start, &buf32)?;

    Ok(())
}

pub fn flush<AP>(interface: &mut AP) -> Result<(), AccessPortError>
where
    AP: CommunicationInterface
        + APAccess<MemoryAP, CSW>
        + APAccess<MemoryAP, TAR>
        + APAccess<MemoryAP, DRW>,
{
    match interface.flush() {
        Ok(_) => Ok(()),
        Err(e) => Err(AccessPortError::FlushError(e)),
    }
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
    use super::super::super::ap::memory_ap::mock::MockMemoryAP;

    // Visually obvious pattern used to test memory writes
    const DATA8: &[u8] = &[
        128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
    ];

    // DATA8 interpreted as little endian 32-bit words
    const DATA32: &[u32] = &[0x83828180, 0x87868584, 0x8b8a8988, 0x8f8e8d8c];

    #[test]
    fn read_word_32() {
        let mut mock = MockMemoryAP::with_pattern();
        mock.memory[..8].copy_from_slice(&DATA8[..8]);

        for &address in &[0, 4] {
            let value = super::read_word_32(&mut mock, 0, address).expect("read_word_32 failed");
            assert_eq!(value, DATA32[address as usize / 4]);
        }
    }

    #[test]
    fn read_word_8() {
        let mut mock = MockMemoryAP::with_pattern();
        mock.memory[..8].copy_from_slice(&DATA8[..8]);

        for address in 0..8 {
            let value = super::read_word_8(&mut mock, 0, address)
                .unwrap_or_else(|_| panic!("read_word_8 failed, address = {}", address));
            assert_eq!(value, DATA8[address as usize], "address = {}", address);
        }
    }

    #[test]
    fn write_word_32() {
        for &address in &[0, 4] {
            let mut mock = MockMemoryAP::with_pattern();

            let mut expected = mock.memory.clone();
            expected[(address as usize)..(address as usize) + 4].copy_from_slice(&DATA8[..4]);

            super::write_word_32(&mut mock, 0, address, DATA32[0])
                .unwrap_or_else(|_| panic!("write_word_32 failed, address = {}", address));
            assert_eq!(mock.memory, expected.as_slice(), "address = {}", address);
        }
    }

    #[test]
    fn write_word_8() {
        for address in 0..8 {
            let mut mock = MockMemoryAP::with_pattern();

            let mut expected = mock.memory.clone();
            expected[address] = DATA8[0];

            super::write_word_8(&mut mock, 0, address as u32, DATA8[0])
                .unwrap_or_else(|_| panic!("write_word_8 failed, address = {}", address));
            assert_eq!(mock.memory, expected.as_slice(), "address = {}", address);
        }
    }

    #[test]
    fn read_32() {
        let mut mock = MockMemoryAP::with_pattern();
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);

        for &address in &[0, 4] {
            for len in 0..3 {
                let mut data = vec![0u32; len];
                super::read_32(&mut mock, 0, address, &mut data).unwrap_or_else(|_| {
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
        let mut mock = MockMemoryAP::with_pattern();

        for &address in &[1, 3, 127] {
            assert!(super::read_32(&mut mock, 0, address, &mut [0u32; 4]).is_err());
        }
    }

    #[test]
    fn read_8() {
        let mut mock = MockMemoryAP::with_pattern();
        mock.memory[..DATA8.len()].copy_from_slice(DATA8);

        for address in 0..4 {
            for len in 0..12 {
                let mut data = vec![0u8; len];
                super::read_8(&mut mock, 0, address, &mut data).unwrap_or_else(|_| {
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
                let mut mock = MockMemoryAP::with_pattern();

                let mut expected = mock.memory.clone();
                expected[address as usize..(address as usize) + len * 4]
                    .copy_from_slice(&DATA8[..len * 4]);

                let data = &DATA32[..len];
                super::write_32(&mut mock, 0, address, data).unwrap_or_else(|_| {
                    panic!("write_32 failed, address = {}, len = {}", address, len)
                });

                assert_eq!(
                    mock.memory,
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
        let mut mock = MockMemoryAP::with_pattern();

        for &address in &[1, 3, 127] {
            assert!(super::write_32(&mut mock, 0, address, &[0xDEAD_BEEF, 0xABBA_BABE]).is_err());
        }
    }

    #[test]
    fn write_8() {
        for address in 0..4 {
            for len in 0..12 {
                let mut mock = MockMemoryAP::with_pattern();

                let mut expected = mock.memory.clone();
                expected[address as usize..(address as usize) + len].copy_from_slice(&DATA8[..len]);

                let data = &DATA8[..len];
                super::write_8(&mut mock, 0, address, data).unwrap_or_else(|_| {
                    panic!("write_8 failed, address = {}, len = {}", address, len)
                });

                assert_eq!(
                    mock.memory,
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
