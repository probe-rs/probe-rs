use super::super::{APAccess, Register};
use super::{APRegister, AddressIncrement, DataSize, MemoryAP, CSW, DRW, TAR};
use crate::{
    architecture::arm::dp::{DPAccess, DPRegister, DebugPortError},
    CommunicationInterface, DebugProbeError,
};
use std::collections::HashMap;
use std::convert::TryInto;
use thiserror::Error;

#[derive(Debug)]
pub struct MockMemoryAP {
    pub memory: Vec<u8>,
    store: HashMap<(u8, u8), u32>,
}

#[derive(Debug, Error)]
pub enum MockMemoryError {
    #[error("Unknown register width")]
    UnknownWidth,
    #[error("Unknown register")]
    UnknownRegister,
}

impl MockMemoryAP {
    /// Creates a MockMemoryAP with the memory filled with a pattern where each byte is equal to its
    /// own address plus one (to avoid zeros). The pattern can be used as a canary pattern to ensure
    /// writes do not clobber adjacent memory. The memory is also quite small so it can be feasibly
    /// printed out for debugging.
    pub fn with_pattern() -> Self {
        let mut store = HashMap::new();
        store.insert((CSW::ADDRESS, CSW::APBANKSEL), 0);
        store.insert((TAR::ADDRESS, TAR::APBANKSEL), 0);
        store.insert((DRW::ADDRESS, DRW::APBANKSEL), 0);
        Self {
            memory: (1..=16).collect(),
            store,
        }
    }
}

impl CommunicationInterface for MockMemoryAP {
    fn flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

impl<R> APAccess<MemoryAP, R> for MockMemoryAP
where
    R: APRegister<MemoryAP>,
{
    type Error = MockMemoryError;

    /// Mocks the read_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_ap_register(
        &mut self,
        _port: impl Into<MemoryAP>,
        _register: R,
    ) -> Result<R, Self::Error> {
        let csw = self.store[&(CSW::ADDRESS, CSW::APBANKSEL)];
        let address = self.store[&(TAR::ADDRESS, TAR::APBANKSEL)];

        match (R::ADDRESS, R::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => {
                let drw = self.store[&(DRW::ADDRESS, DRW::APBANKSEL)];
                let bit_offset = (address % 4) * 8;
                let offset = address as usize;
                let csw = CSW::from(csw);

                let new_drw = match csw.SIZE {
                    DataSize::U32 => {
                        let bytes: [u8; 4] = self
                            .memory
                            .get(offset..offset + 4)
                            .map(|v| v.try_into().unwrap())
                            .unwrap_or([0u8; 4]);

                        u32::from_le_bytes(bytes)
                    }
                    DataSize::U16 => {
                        let bytes = self
                            .memory
                            .get(offset..offset + 2)
                            .map(|v| v.try_into().unwrap())
                            .unwrap_or([0u8; 2]);
                        let value = u16::from_le_bytes(bytes);
                        drw & !(0xffff << bit_offset) | (u32::from(value) << bit_offset)
                    }
                    DataSize::U8 => {
                        let value = *self.memory.get(offset).unwrap_or(&0u8);
                        drw & !(0xff << bit_offset) | (u32::from(value) << bit_offset)
                    }
                    _ => return Err(MockMemoryError::UnknownWidth),
                };

                self.store.insert((DRW::ADDRESS, DRW::APBANKSEL), new_drw);

                match csw.AddrInc {
                    AddressIncrement::Single => {
                        let new_address = match csw.SIZE {
                            DataSize::U32 => address + 4,
                            DataSize::U16 => address + 2,
                            DataSize::U8 => address + 1,
                            _ => unimplemented!(),
                        };

                        self.store
                            .insert((TAR::ADDRESS, TAR::APBANKSEL), new_address);
                    }
                    AddressIncrement::Off => (),
                    AddressIncrement::Packed => {
                        unimplemented!();
                    }
                }

                Ok(R::from(new_drw))
            }
            (CSW::ADDRESS, CSW::APBANKSEL) => Ok(R::from(self.store[&(R::ADDRESS, R::APBANKSEL)])),
            (TAR::ADDRESS, TAR::APBANKSEL) => Ok(R::from(self.store[&(R::ADDRESS, R::APBANKSEL)])),
            _ => Err(MockMemoryError::UnknownRegister),
        }
    }

    /// Mocks the write_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_ap_register(
        &mut self,
        _port: impl Into<MemoryAP>,
        register: R,
    ) -> Result<(), Self::Error> {
        log::debug!("Mock: Write to register {:x?}", &register);

        let value = register.into();
        self.store.insert((R::ADDRESS, R::APBANKSEL), value);
        let csw = self.store[&(CSW::ADDRESS, CSW::APBANKSEL)];
        let address = self.store[&(TAR::ADDRESS, TAR::APBANKSEL)];

        match (R::ADDRESS, R::APBANKSEL) {
            (DRW::ADDRESS, DRW::APBANKSEL) => {
                let csw = CSW::from(csw);

                let access_width = match csw.SIZE {
                    DataSize::U256 => 32,
                    DataSize::U128 => 16,
                    DataSize::U64 => 8,
                    DataSize::U32 => 4,
                    DataSize::U16 => 2,
                    DataSize::U8 => 1,
                };

                if (address + access_width) as usize > self.memory.len() {
                    // Ignore out-of-bounds write
                    return Ok(());
                }

                let bit_offset = (address % 4) * 8;
                let result = match csw.SIZE {
                    DataSize::U32 => {
                        self.memory[address as usize] = value as u8;
                        self.memory[address as usize + 1] = (value >> 8) as u8;
                        self.memory[address as usize + 2] = (value >> 16) as u8;
                        self.memory[address as usize + 3] = (value >> 24) as u8;
                        Ok(())
                    }
                    DataSize::U16 => {
                        let value = value >> bit_offset;
                        self.memory[address as usize] = value as u8;
                        self.memory[address as usize + 1] = (value >> 8) as u8;
                        Ok(())
                    }
                    DataSize::U8 => {
                        let value = value >> bit_offset;
                        self.memory[address as usize] = value as u8;
                        Ok(())
                    }
                    _ => Err(MockMemoryError::UnknownWidth),
                };

                if result.is_ok() {
                    match csw.AddrInc {
                        AddressIncrement::Single => {
                            let new_address = match csw.SIZE {
                                DataSize::U32 => address + 4,
                                DataSize::U16 => address + 2,
                                DataSize::U8 => address + 1,
                                _ => unimplemented!(),
                            };
                            self.store
                                .insert((TAR::ADDRESS, TAR::APBANKSEL), new_address);
                        }
                        AddressIncrement::Off => (),
                        AddressIncrement::Packed => {
                            unimplemented!();
                        }
                    }
                }

                result
            }
            (CSW::ADDRESS, CSW::APBANKSEL) => {
                self.store.insert((CSW::ADDRESS, CSW::APBANKSEL), value);
                Ok(())
            }
            (TAR::ADDRESS, TAR::APBANKSEL) => {
                self.store.insert((TAR::ADDRESS, TAR::APBANKSEL), value);
                Ok(())
            }
            _ => Err(MockMemoryError::UnknownRegister),
        }
    }

    fn write_ap_register_repeated(
        &mut self,
        port: impl Into<MemoryAP> + Clone,
        _register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        for value in values {
            self.write_ap_register(port.clone(), R::from(*value))?
        }

        Ok(())
    }
    fn read_ap_register_repeated(
        &mut self,
        port: impl Into<MemoryAP> + Clone,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        for value in values {
            *value = self
                .read_ap_register(port.clone(), register.clone())?
                .into()
        }

        Ok(())
    }
}

impl DPAccess for MockMemoryAP {
    fn read_dp_register<R: DPRegister>(&mut self) -> Result<R, DebugPortError> {
        // Ignore for Tests
        Ok(0.into())
    }

    fn write_dp_register<R: DPRegister>(&mut self, _register: R) -> Result<(), DebugPortError> {
        Ok(())
    }
}
