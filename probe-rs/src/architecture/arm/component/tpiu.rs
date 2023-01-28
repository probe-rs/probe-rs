use super::super::memory::romtable::CoresightComponent;
use crate::architecture::arm::ArmProbeInterface;
use crate::Error;

pub const _TPIU_PID: [u8; 8] = [0xA1, 0xB9, 0x0B, 0x0, 0x4, 0x0, 0x0, 0x0];

const _REGISTER_OFFSET_TPIU_SSPSR: u32 = 0x0;
const REGISTER_OFFSET_TPIU_CSPSR: u32 = 0x4;
const REGISTER_OFFSET_TPIU_ACPR: u32 = 0x10;
const REGISTER_OFFSET_TPIU_SPPR: u32 = 0xF0;
const REGISTER_OFFSET_TPIU_FFCR: u32 = 0x304;

/// TPIU unit
///
/// Trace port interface unit unit.
pub struct Tpiu<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> Tpiu<'a> {
    /// Create a new TPIU interface from a probe and a ROM table component.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Tpiu {
            interface,
            component,
        }
    }

    /// Set the port size of the TPIU.
    pub fn set_port_size(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_TPIU_CSPSR, value)?;
        Ok(())
    }

    /// Set the prescaler of the TPIU.
    pub fn set_prescaler(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_TPIU_ACPR, value)?;
        Ok(())
    }

    /// Set the TPIU protocol.
    /// 0 = sync trace mode
    /// 1 = async SWO (manchester)
    /// 2 = async SWO (NRZ)
    /// 3 = reserved
    pub fn set_pin_protocol(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_TPIU_SPPR, value)?;
        Ok(())
    }

    /// Set the TPIU formatter.
    pub fn set_formatter(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_TPIU_FFCR, value)?;
        Ok(())
    }
}
