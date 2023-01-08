use super::super::{ApAccess, Register};
use super::{AddressIncrement, ApRegister, DataSize, CSW, DRW, TAR};
use crate::architecture::arm::communication_interface::FlushableArmAccess;
use crate::architecture::arm::{
    ap::AccessPort,
    dp::{DpAccess, DpRegister},
    ArmError, DpAddress,
};
use crate::DebugProbeError;
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
            memory: std::iter::repeat(1..=255).flatten().take(1 << 15).collect(),
            store,
        }
    }
}

impl FlushableArmAccess for MockMemoryAp {
    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<
        &mut crate::architecture::arm::ArmCommunicationInterface<
            crate::architecture::arm::communication_interface::Initialized,
        >,
        DebugProbeError,
    > {
        todo!()
    }
}

impl ApAccess for MockMemoryAp {
    /// Mocks the read_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn read_ap_register<PORT, R>(&mut self, _port: impl Into<PORT>) -> Result<R, ArmError>
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
                let csw = CSW::try_from(csw).unwrap();

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
                    _ => panic!("MockMemoryAp: unknown width"),
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

                Ok(R::try_from(new_drw).unwrap())
            }
            CSW::ADDRESS => Ok(R::try_from(self.store[&R::ADDRESS]).unwrap()),
            TAR::ADDRESS => Ok(R::try_from(self.store[&R::ADDRESS]).unwrap()),
            _ => panic!("MockMemoryAp: unknown register"),
        }
    }

    /// Mocks the write_register method of a AP.
    ///
    /// Returns an Error if any bad instructions or values are chosen.
    fn write_ap_register<PORT, R>(
        &mut self,
        _port: impl Into<PORT>,
        register: R,
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        tracing::debug!("Mock: Write to register {:x?}", &register);

        let value: u32 = register.into();
        self.store.insert(R::ADDRESS, value);
        let csw = self.store[&CSW::ADDRESS];
        let address = self.store[&TAR::ADDRESS];

        match R::ADDRESS {
            DRW::ADDRESS => {
                let csw = CSW::try_from(csw).unwrap();

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
                    _ => panic!("MockMemoryAp: unknown width"),
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
            _ => panic!("MockMemoryAp: unknown register"),
        }
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        _register: R,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        for value in values {
            self.write_ap_register(port.clone(), R::try_from(*value).unwrap())?
        }

        Ok(())
    }

    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        _register: R,
        values: &mut [u32],
    ) -> Result<(), ArmError>
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
    fn read_dp_register<R: DpRegister>(&mut self, _dp: DpAddress) -> Result<R, ArmError> {
        // Ignore for Tests
        Ok(0.try_into().unwrap())
    }

    fn write_dp_register<R: DpRegister>(
        &mut self,
        _dp: DpAddress,
        _register: R,
    ) -> Result<(), ArmError> {
        Ok(())
    }
}
