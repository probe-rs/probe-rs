//! Helpers for testing the crate

use crate::MemoryInterface;

#[derive(Debug)]
pub(crate) struct MockMemory {
    /// Sorted list of ranges
    values: Vec<(u64, Vec<u8>)>,
}

impl MockMemory {
    pub(crate) fn new() -> Self {
        MockMemory { values: Vec::new() }
    }

    pub(crate) fn add_range(&mut self, address: u64, data: Vec<u8>) {
        assert!(!data.is_empty());

        match self
            .values
            .binary_search_by_key(&address, |(addr, _data)| *addr)
        {
            Ok(index) => {
                panic!("Failed to add data at {:#010x} - {:#010x}, already exists at {:#010x} - {:#010x}", address, address + data.len() as u64, self.values[index].0, self.values[index].0 + self.values[index].1.len() as u64);
            }
            Err(index) => {
                // This is the index where the new entry should be inserted,
                // but we first have to check on both sides, if this would overlap with existing entries

                if index > 0 {
                    let previous_entry = &self.values[index - 1];

                    assert!(
                            previous_entry.0 + previous_entry.1.len() as u64 <= address,
                            "Failed to add data at {:#010x} - {:#010x}, overlaps with existing entry at {:#010x} - {:#010x}",
                            address,
                            address + data.len() as u64,
                            previous_entry.0,
                            previous_entry.0 + previous_entry.1.len() as u64
                        );
                }

                if index + 1 < self.values.len() {
                    let next_entry = &self.values[index + 1];

                    assert!(
                            next_entry.0 >= address + data.len() as u64,
                            "Failed to add data at {:#010x} - {:#010x}, overlaps with existing entry at {:#010x} - {:#010x}",
                            address,
                            address + data.len() as u64,
                            next_entry.0,
                            next_entry.0 + next_entry.1.len() as u64
                        );
                }

                self.values.insert(index, (address, data));
            }
        }
    }

    pub(crate) fn add_word_range(&mut self, address: u64, data: &[u32]) {
        let mut bytes = Vec::with_capacity(data.len() * 4);

        for word in data {
            bytes.extend_from_slice(&word.to_le_bytes());
        }

        self.add_range(address, bytes);
    }

    fn missing_range(&self, start: u64, end: u64) -> ! {
        panic!("No entry for range {:#010x} - {:#010x}", start, end);
    }
}

impl MemoryInterface for MockMemory {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, _address: u64) -> Result<u64, crate::Error> {
        todo!()
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut bytes = [0u8; 4];
        self.read_8(address, &mut bytes)?;

        Ok(u32::from_le_bytes(bytes))
    }

    fn read_word_8(&mut self, _address: u64) -> Result<u8, crate::Error> {
        todo!()
    }

    fn read_word_16(&mut self, _address: u64) -> Result<u16, crate::Error> {
        todo!()
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), crate::Error> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        let mut buff = vec![0u8; data.len() * 4];

        self.read_8(address, &mut buff)?;

        for (i, chunk) in buff.chunks_exact(4).enumerate() {
            data[i] = u32::from_le_bytes(chunk.try_into().unwrap());
        }

        Ok(())
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), crate::Error> {
        todo!()
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        let stored_data = match self
            .values
            .binary_search_by_key(&address, |(addr, _data)| *addr)
        {
            Ok(index) => {
                // Found entry with matching start address

                &self.values[index].1
            }
            Err(0) => self.missing_range(address, address + data.len() as u64),
            Err(index) => {
                let previous_entry = &self.values[index - 1];

                // address:        10  - 12
                // previous_entry  8   - 11

                // reading from 10 -> reading from 8 + 2

                let offset = address - previous_entry.0;

                if offset >= previous_entry.1.len() as u64 {
                    // The requested range is not covered by the previous entry
                    self.missing_range(address, address + data.len() as u64)
                }

                &previous_entry.1[offset as usize..]
            }
        };

        if stored_data.len() >= data.len() {
            data.copy_from_slice(&stored_data[..data.len()]);
            Ok(())
        } else {
            data[..stored_data.len()].copy_from_slice(stored_data);

            self.read_8(
                address + stored_data.len() as u64,
                &mut data[stored_data.len()..],
            )
        }
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        Ok(true)
    }

    fn write_word_64(&mut self, _address: u64, _data: u64) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_32(&mut self, _address: u64, _data: u32) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_16(&mut self, _address: u64, _data: u16) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_8(&mut self, _address: u64, _data: u8) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_32(&mut self, _address: u64, _data: &[u32]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), crate::Error> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        todo!()
    }
}

#[test]
fn mock_memory_read() {
    let mut mock_memory = MockMemory::new();

    let values = [
        0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000, 0x0000017f, 0x00000180,
        0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
    ];

    mock_memory.add_word_range(0x2001_ffd0, &values);

    for (offset, expected) in values.iter().enumerate() {
        let actual = mock_memory
            .read_word_32(0x2001_ffd0 + (offset * 4) as u64)
            .unwrap();

        assert_eq!(actual, *expected);
    }
}
