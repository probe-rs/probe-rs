use crate::architecture::arm::{
    ArmError, DapAccess,
    ap::{
        AddressIncrement, ApClass, ApRegister, ApType, CFG, DRW, DataSize, IDR, TAR,
        memory_ap::amba_ahb3::CSW,
    },
    communication_interface::{DapProbe, FlushableArmAccess},
    dp::{DpAddress, DpRegisterAddress},
};
use std::collections::HashMap;

#[derive(Debug)]
pub struct MockMemoryAp {
    pub memory: Vec<u8>,
    store: HashMap<u64, u32>,
}

impl MockMemoryAp {
    /// Creates a MockMemoryAp with the memory filled with a pattern where each byte is equal to its
    /// own address plus one (to avoid zeros). The pattern can be used as a canary pattern to ensure
    /// writes do not clobber adjacent memory.
    pub fn with_pattern() -> Self {
        Self::with_pattern_and_size(1 << 15)
    }

    /// Creates a MockMemoryAp with the given size where the memory filled with a pattern where each
    /// byte is equal to its own address plus one (to avoid zeros). The pattern can be used as a
    /// canary pattern to ensure writes do not clobber adjacent memory.
    pub fn with_pattern_and_size(size: usize) -> Self {
        let mut store = HashMap::new();
        store.insert(
            IDR::ADDRESS,
            IDR {
                REVISION: 0,
                DESIGNER: jep106::JEP106Code::new(4, 0x3b),
                CLASS: ApClass::MemAp,
                _RES0: 0,
                VARIANT: 0,
                TYPE: ApType::AmbaAhb3,
            }
            .into(),
        );
        store.insert(CFG::ADDRESS, 0);
        store.insert(CSW::ADDRESS, 0);
        store.insert(TAR::ADDRESS, 0);
        store.insert(DRW::ADDRESS, 0);
        Self {
            memory: std::iter::repeat(1..=255).flatten().take(size).collect(),
            store,
        }
    }
}

impl FlushableArmAccess for MockMemoryAp {
    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }
}

impl DapAccess for MockMemoryAp {
    fn read_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _addr: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        // Ignore for Tests
        Ok(0)
    }

    fn write_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _addr: DpRegisterAddress,
        _value: u32,
    ) -> Result<(), ArmError> {
        Ok(())
    }

    fn read_raw_ap_register(
        &mut self,
        _ap: &crate::architecture::arm::FullyQualifiedApAddress,
        addr: u64,
    ) -> Result<u32, ArmError> {
        let csw = self.store[&CSW::ADDRESS];
        let address = self.store[&TAR::ADDRESS];

        tracing::debug!("Reading: addr {:x} store: {:x?}", addr, self.store);

        if addr == DRW::ADDRESS {
            let drw = self.store[&DRW::ADDRESS];
            let bit_offset = (address % 4) * 8;
            let offset = address as usize;
            let csw = CSW::try_from(csw).unwrap();

            let (new_drw, offset) = match csw.Size {
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
            tracing::debug!("Reading: new store: {:x?}", self.store);

            Ok(new_drw)
        } else {
            Ok(self
                .store
                .get(&addr)
                .cloned()
                .expect("MockMemoryAp: unknown register"))
        }
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: &crate::architecture::arm::FullyQualifiedApAddress,
        addr: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        tracing::debug!("Mock: Write {:x} to register {:x?}", value, &addr);

        self.store.insert(addr, value);
        let csw = self.store[&CSW::ADDRESS];
        let address = self.store[&TAR::ADDRESS];

        match addr {
            DRW::ADDRESS => {
                let csw = CSW::try_from(csw).unwrap();
                tracing::debug!("csw: {:x?}", csw);

                let access_width = csw.Size.to_byte_count() as u32;

                if (address + access_width) as usize > self.memory.len() {
                    // Ignore out-of-bounds write
                    return Ok(());
                }

                let bit_offset = (address % 4) * 8;
                match csw.Size {
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

    fn try_dap_probe(&self) -> Option<&dyn DapProbe> {
        None
    }

    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe> {
        None
    }
}
