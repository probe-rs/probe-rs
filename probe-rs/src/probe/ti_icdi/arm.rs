use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::MemoryInterface;
use crate::architecture::arm::ap::{AccessPortType, ApRegister, CFG, CSW, IDR, MemoryAp};
use crate::architecture::arm::communication_interface::SwdSequence;
use crate::architecture::arm::dp::{DpAddress, DpRegisterAddress};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::{
    ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress, SwoAccess, SwoConfig,
    valid_32bit_arm_address,
};
use crate::probe::ti_icdi::IcdiProbe;
use crate::probe::ti_icdi::gdb_interface::GdbRemoteInterface;
use crate::probe::{BitSequence, DebugProbeError, Probe};
use zerocopy::IntoBytes;

/// AHB3 MEM-AP IDR used to satisfy [`MemoryAp::new`] for a probe that has no DAP.
const FAKE_IDR: u32 = 0x24770031;
const FAKE_CSW: u32 = 0x23000052;
const FAKE_CFG: u32 = 0x00000000;

/// Cortex-M CPUID register.
const CPUID: u64 = 0xE000_ED00;
/// Peripheral Bus ROM table used by Cortex-M3 and later M-class cores.
const CORTEX_M_ROM_TABLE: u64 = 0xE00F_F000;
const ARM_IMPLEMENTER: u32 = 0x41;

/// ARM debug interface for [`IcdiProbe`].
///
/// ICDI does not expose DAP, so DP/AP register access is stubbed and memory
/// is read and written through the GDB remote protocol.
#[derive(Debug)]
pub(crate) struct IcdiArmDebug {
    probe: Box<IcdiProbe>,
    is_connected_to_dp: bool,
    rom_table_base: Option<u64>,
    _sequence: Arc<dyn ArmDebugSequence>,
}

impl IcdiArmDebug {
    pub fn new(probe: Box<IcdiProbe>, sequence: Arc<dyn ArmDebugSequence>) -> Self {
        Self {
            probe,
            is_connected_to_dp: false,
            rom_table_base: None,
            _sequence: sequence,
        }
    }
}

/// Map a CPUID value to the architectural PPB ROM table for cores that have one.
fn rom_table_from_cpuid(cpuid: u32) -> Option<u64> {
    let implementer = (cpuid >> 24) & 0xFF;
    let partno = (cpuid >> 4) & 0xFFF;
    if implementer != ARM_IMPLEMENTER {
        return None;
    }

    match partno {
        // Cortex-M3, M4, M7, M33, M35P, M55, M85, M52
        0xC23 | 0xC24 | 0xC27 | 0xD21 | 0xD31 | 0xD22 | 0xD23 | 0xD24 => Some(CORTEX_M_ROM_TABLE),
        _ => None,
    }
}

impl DapAccess for IcdiArmDebug {
    fn read_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _addr: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        Err(ArmError::NotImplemented("dp register read not implemented"))
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
        _ap: &FullyQualifiedApAddress,
        addr: u64,
    ) -> Result<u32, ArmError> {
        if addr == IDR::ADDRESS {
            Ok(FAKE_IDR)
        } else if addr == CSW::ADDRESS {
            Ok(FAKE_CSW)
        } else if addr == CFG::ADDRESS {
            Ok(FAKE_CFG)
        } else {
            Err(ArmError::NotImplemented("ap register read not implemented"))
        }
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _addr: u64,
        _value: u32,
    ) -> Result<(), ArmError> {
        Ok(())
    }
}

impl SwdSequence for IcdiArmDebug {
    fn swj_sequence(&mut self, _bits: &BitSequence) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "swj_sequence",
        })
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "swj_pins",
        })
    }
}

impl SwoAccess for IcdiArmDebug {
    fn enable_swo(&mut self, _config: &SwoConfig) -> Result<(), ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }

    fn read_swo_timeout(&mut self, _timeout: Duration) -> Result<Vec<u8>, ArmError> {
        Err(ArmError::NotImplemented("swo not implemented"))
    }
}

impl ArmDebugInterface for IcdiArmDebug {
    fn reinitialize(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        if dp != DpAddress::Default {
            return Err(ArmError::NotImplemented("multidrop not implemented"));
        }
        Ok(BTreeSet::from([
            FullyQualifiedApAddress::v1_with_default_dp(0),
        ]))
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }

    fn current_debug_port(&self) -> Option<DpAddress> {
        if self.is_connected_to_dp {
            Some(DpAddress::Default)
        } else {
            None
        }
    }

    fn select_debug_port(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        if dp != DpAddress::Default {
            return Err(ArmError::NotImplemented("multidrop not implemented"));
        }
        self.is_connected_to_dp = true;
        Ok(())
    }

    fn memory_interface(
        &mut self,
        access_port: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        let memory_ap = MemoryAp::new(self, access_port)?;
        Ok(Box::new(IcdiMemoryInterface {
            probe: self,
            current_ap: memory_ap,
        }))
    }

    fn active_wire_protocol(&self) -> Option<crate::probe::WireProtocol> {
        Some(self.probe.protocol)
    }

    fn wire_speed_khz(&self) -> Option<u32> {
        Some(self.probe.speed_khz)
    }
}

#[derive(Debug)]
struct IcdiMemoryInterface<'probe> {
    probe: &'probe mut IcdiArmDebug,
    current_ap: MemoryAp,
}

impl IcdiMemoryInterface<'_> {
    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        let address = valid_32bit_arm_address(address)?;
        self.probe
            .probe
            .device
            .read_mem(address, data)
            .map_err(ArmError::from)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        let address = valid_32bit_arm_address(address)?;
        self.probe
            .probe
            .device
            .write_mem(address, data)
            .map_err(ArmError::from)
    }
}

impl ArmMemoryInterface for IcdiMemoryInterface<'_> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.current_ap.ap_address().clone()
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        if let Some(base) = self.probe.rom_table_base {
            return Ok(base);
        }

        let mut cpuid = [0u32; 1];
        self.read_32(CPUID, &mut cpuid)?;
        let Some(base) = rom_table_from_cpuid(cpuid[0]) else {
            return Err(ArmError::Other(format!(
                "unsupported CPUID {:#010x} for CoreSight ROM table discovery",
                cpuid[0]
            )));
        };
        self.probe.rom_table_base = Some(base);
        Ok(base)
    }

    fn get_arm_debug_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError> {
        Ok(self.probe)
    }

    fn generic_status(&mut self) -> Result<CSW, ArmError> {
        CSW::try_from(FAKE_CSW).map_err(ArmError::RegisterParse)
    }
}

impl MemoryInterface<ArmError> for IcdiMemoryInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        self.read(address, data.as_mut_bytes())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        self.read(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        self.write(address, data.as_bytes())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        self.write(address, data.as_bytes())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        self.write(address, data.as_bytes())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        self.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }
}
