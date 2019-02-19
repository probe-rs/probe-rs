use crate::access_ports::APRegister;
use crate::access_ports::APValue;
use crate::access_ports::APType;

pub struct MemoryAP {
    port_numer: u8,
}

impl APType for MemoryAP {
    fn get_port_number(&self) -> u8 {
        self.port_numer
    }
}

#[derive(Clone, Copy)]
pub enum MemoryAPRegister {
    Mock = 0xBABE
}

impl APRegister for MemoryAPRegister {
    fn to_u32(&self) -> u32 {
        *self as u32
    }

    fn to_value<MemoryAPValue>(&self) -> MemoryAPValue {
        match self {
            MemoryAPRegister::Mock => MemoryAPValue::Mock(Mock { x: 0, y: 0 })
        }
    }
}

pub struct Mock {
    x: u16,
    y: u16,
}

pub enum MemoryAPValue {
    Mock(Mock),
}

impl APValue for MemoryAPValue {
    fn from_u32(self, value: u32) -> Self {
        match self {
            MemoryAPValue::Mock(m) => MemoryAPValue::Mock(Mock {
                x: (value >> 16) as u16,
                y: value as u16
            }),
        }
    }

    fn to_u32(&self) -> u32 {
        match self {
            MemoryAPValue::Mock(v) => (v.x << 16) as u32 | v.y as u32,
        }
    }
}