use anyhow::anyhow;

use super::super::{ApAccess, Register};
use super::{AddressIncrement, ApRegister, DataSize, CSW, DRW, TAR};
use crate::architecture::arm::{ap::AccessPort, DpAddress};
use crate::{
    architecture::arm::dp::{DebugPortError, DpAccess, DpRegister},
    CommunicationInterface, DebugProbeError,
};
use std::collections::HashMap;
use std::convert::TryInto;

#[derive(Debug)]
pub struct MockMemoryAp {
    pub memory: Vec<u8>,
    store: HashMap<u8, u32>,
}

impl MockMemoryAp {
    /// Creates a MockMemoryAp with the memory filled with a pattern where each byte is equal to its
    /// own address plus one (to avoid zeros). The pattern can be used as a canary pattern to ensure
    /// writes do not clobber adjacent memory. The memory is also quite small so it can be feasibly
    /// printed out for debugging.
    pub fn with_pattern() -> Self {
        let mut store = HashMap::new();
        store.insert(CSW::ADDRESS, 0);
        store.insert(TAR::ADDRESS, 0);
        store.insert(DRW::ADDRESS, 0);
        Self {
            memory: (1..=16).collect(),
            store,
        }
    }
}

impl CommunicationInterface for MockMemoryAp {
    fn flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

impl ApAccess for MockMemoryAp {
    /// Mocks the read_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_ap_register<PORT, R>(&mut self, _port: impl Into<PORT>) -> Result<R, DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        let csw = self.store[&CSW::ADDRESS];
        let address = self.store[&TAR::ADDRESS];

        match R::ADDRESS {
            DRW::ADDRESS => {
                let drw = self.store[&DRW::ADDRESS];
                let bit_offset = (address % 4) * 8;
                let offset = address as usize;
                let csw = CSW::from(csw);

                let (new_drw, offset) = match csw.SIZE {
                    DataSize::U32 => {
                        let bytes: [u8; 4] = self
                            .memory
                            .get(offset..offset + 4)
                            .map(|v| v.try_into().unwrap())
                            .unwrap_or([0u8; 4]);

                        (u32::from_le_bytes(bytes), 4)
                    }
                    DataSize::U16 => {
                        let bytes = self
                            .memory
                            .get(offset..offset + 2)
                            .map(|v| v.try_into().unwrap())
                            .unwrap_or([0u8; 2]);
                        let value = u16::from_le_bytes(bytes);
                        (
                            drw & !(0xffff << bit_offset) | (u32::from(value) << bit_offset),
                            2,
                        )
                    }
                    DataSize::U8 => {
                        let value = *self.memory.get(offset).unwrap_or(&0u8);
                        (
                            drw & !(0xff << bit_offset) | (u32::from(value) << bit_offset),
                            1,
                        )
                    }
                    _ => Err(anyhow!("MockMemoryAp: unknown width"))?,
                };

                self.store.insert(DRW::ADDRESS, new_drw);

                match csw.AddrInc {
                    AddressIncrement::Single => {
                        self.store.insert(TAR::ADDRESS, address + offset);
                    }
                    AddressIncrement::Off => (),
                    AddressIncrement::Packed => {
                        unimplemented!();
                    }
                }

                Ok(R::from(new_drw))
            }
            CSW::ADDRESS => Ok(R::from(self.store[&R::ADDRESS])),
            TAR::ADDRESS => Ok(R::from(self.store[&R::ADDRESS])),
            _ => Err(anyhow!("MockMemoryAp: unknown register"))?,
        }
    }

    /// Mocks the write_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_ap_register<PORT, R>(
        &mut self,
        _port: impl Into<PORT>,
        register: R,
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        log::debug!("Mock: Write to register {:x?}", &register);

        let value: u32 = register.into();
        self.store.insert(R::ADDRESS, value);
        let csw = self.store[&CSW::ADDRESS];
        let address = self.store[&TAR::ADDRESS];

        match R::ADDRESS {
            DRW::ADDRESS => {
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
                match csw.SIZE {
                    DataSize::U32 => {
                        self.memory[address as usize..address as usize + 4]
                            .copy_from_slice(&value.to_le_bytes());
                        Ok(4)
                    }
                    DataSize::U16 => {
                        let value = value >> bit_offset;
                        self.memory[address as usize] = value as u8;
                        self.memory[address as usize + 1] = (value >> 8) as u8;
                        Ok(2)
                    }
                    DataSize::U8 => {
                        let value = value >> bit_offset;
                        self.memory[address as usize] = value as u8;
                        Ok(1)
                    }
                    _ => Err(anyhow!("MockMemoryAp: unknown width"))?,
                }
                .map(|offset| match csw.AddrInc {
                    AddressIncrement::Single => {
                        self.store.insert(TAR::ADDRESS, address + offset);
                    }
                    AddressIncrement::Off => (),
                    AddressIncrement::Packed => {
                        unimplemented!();
                    }
                })
            }
            CSW::ADDRESS => {
                self.store.insert(CSW::ADDRESS, value);
                Ok(())
            }
            TAR::ADDRESS => {
                self.store.insert(TAR::ADDRESS, value);
                Ok(())
            }
            _ => Err(anyhow!("MockMemoryAp: unknown register"))?,
        }
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        _register: R,
        values: &[u32],
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        for value in values {
            self.write_ap_register(port.clone(), R::from(*value))?
        }

        Ok(())
    }

    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        _register: R,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        for value in values {
            let register_value: R = self.read_ap_register(port.clone())?;
            *value = register_value.into()
        }

        Ok(())
    }
}

impl DpAccess for MockMemoryAp {
    fn read_dp_register<R: DpRegister>(&mut self, _dp: DpAddress) -> Result<R, DebugPortError> {
        // Ignore for Tests
        Ok(0.into())
    }

    fn write_dp_register<R: DpRegister>(
        &mut self,
        _dp: DpAddress,
        _register: R,
    ) -> Result<(), DebugPortError> {
        Ok(())
    }
}
