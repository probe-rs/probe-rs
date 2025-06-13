use crate::CoreStatus;
use crate::MemoryInterface;
use crate::architecture::arm::ap::{
    AccessPortType, ApRegister, CFG, CSW, IDR, MemoryAp, MemoryApType,
};
use crate::architecture::arm::communication_interface::{DapProbe, SwdSequence};
use crate::architecture::arm::dp::{DpAddress, DpRegisterAddress};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::{
    ArmError, ArmDebugInterface, DapAccess, FullyQualifiedApAddress, SwoAccess, SwoConfig,
};
use crate::probe::sifliuart::{SifliUart, SifliUartCommand, SifliUartResponse};
use crate::probe::{DebugProbeError, Probe};
use std::cmp::{max, min};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use zerocopy::IntoBytes;

#[derive(Debug)]
pub(crate) struct SifliUartArmDebug {
    probe: Box<SifliUart>,

    /// Multidrop is not yet supported for the probe,
    /// so we only keep track if we are connected or not.
    pub is_connected_to_dp: bool,

    /// Information about the APs of the target.
    /// APs are identified by a number, starting from zero.
    pub _access_ports: BTreeSet<FullyQualifiedApAddress>,

    /// A copy of the sequence that was passed during initialization
    _sequence: Arc<dyn ArmDebugSequence>,
}

impl SifliUartArmDebug {
    pub fn new(probe: Box<SifliUart>, sequence: Arc<dyn ArmDebugSequence>) -> Self {
        Self {
            probe,
            is_connected_to_dp: false,
            _access_ports: BTreeSet::new(),
            _sequence: sequence,
        }
    }
}

impl DapAccess for SifliUartArmDebug {
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
        // Fake a MEM-AP's IDR registers
        if addr == IDR::ADDRESS {
            let idr = 0x24770031;
            return Ok(idr);
        } else if addr == CSW::ADDRESS {
            return Ok(0x23000052);
        } else if addr == CFG::ADDRESS {
            return Ok(0x00000000);
        }
        Err(ArmError::NotImplemented("ap register read not implemented"))
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _addr: u64,
        _value: u32,
    ) -> Result<(), ArmError> {
        Ok(())
    }

    fn try_dap_probe(&self) -> Option<&dyn DapProbe> {
        None
    }

    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe> {
        None
    }
}

impl SwdSequence for SifliUartArmDebug {
    fn swj_sequence(&mut self, _bit_len: u8, _bits: u64) -> Result<(), DebugProbeError> {
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

impl SwoAccess for SifliUartArmDebug {
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

impl ArmMemoryInterface for SifliUartMemoryInterface<'_> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.current_ap.ap_address().clone()
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        self.current_ap.base_address(self.probe)
    }

    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError> {
        Ok(self.probe)
    }

    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError> {
        Ok(self.probe)
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        Ok(self.probe)
    }

    fn generic_status(&mut self) -> Result<CSW, ArmError> {
        Err(ArmError::Probe(DebugProbeError::InterfaceNotAvailable {
            interface_name: "ARM",
        }))
    }

    fn update_core_status(&mut self, state: CoreStatus) {
        // If the core status is unknown, we will enter debug mode.
        if state == CoreStatus::Unknown {
            self.probe.probe.command(SifliUartCommand::Enter).unwrap();
        }
    }
}

impl ArmDebugInterface for SifliUartArmDebug {
    fn reinitialize(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn access_ports(
        &mut self,
        _dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        Err(ArmError::NotImplemented("access_ports not implemented"))
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
        let interface = SifliUartMemoryInterface {
            probe: self,
            current_ap: memory_ap,
        };

        Ok(Box::new(interface) as _)
    }
}

#[derive(Debug)]
struct SifliUartMemoryInterface<'probe> {
    probe: &'probe mut SifliUartArmDebug,
    current_ap: MemoryAp,
}

impl SifliUartMemoryInterface<'_> {
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        let sifli_uart = &mut self.probe.probe;

        if data.is_empty() {
            return Ok(());
        }

        let address = if (address & 0xff000000) == 0x12000000 {
            (address & 0x00ffffff) | 0x62000000
        } else {
            address
        };

        let addr_usize = address as usize;
        // Calculate the start address and end address after alignment
        let start_aligned = addr_usize - (addr_usize % 4);
        let end_aligned = (addr_usize + data.len()).div_ceil(4) * 4;
        let total_bytes = end_aligned - start_aligned;
        let total_words = total_bytes / 4;

        let mut buffer = vec![0u8; total_bytes];

        for i in 0..total_words {
            let block_addr = start_aligned + i * 4;
            let block_end = block_addr + 4;

            // Determine if the current 4-byte block is ‘completely overwritten’ by the new data written to it
            // If the block is completely in the new data area, then copy the new data directly
            if block_addr >= addr_usize && block_end <= addr_usize + data.len() {
                let offset_in_data = block_addr - addr_usize;
                buffer[i * 4..i * 4 + 4].copy_from_slice(&data[offset_in_data..offset_in_data + 4]);
            } else {
                // For the rest of the cases (header or tail incomplete overwrite):
                // Call MEMRead first to read out the original 4-byte block.
                let resp = sifli_uart
                    .command(SifliUartCommand::MEMRead {
                        addr: block_addr as u32,
                        len: 1,
                    })
                    .map_err(|e| ArmError::Other(format!("{:?}", e)))?;
                let mut block: [u8; 4] = match resp {
                    SifliUartResponse::MEMRead { data: d } if d.len() == 4 => {
                        [d[0], d[1], d[2], d[3]]
                    }
                    _ => return Err(ArmError::Other("MEMRead Error".to_string())),
                };
                // Calculate the overlap of the block with the new data area
                let overlap_start = max(block_addr, addr_usize);
                let overlap_end = min(block_end, addr_usize + data.len());
                if overlap_start < overlap_end {
                    let in_block_offset = overlap_start - block_addr;
                    let in_data_offset = overlap_start - addr_usize;
                    let overlap_len = overlap_end - overlap_start;
                    block[in_block_offset..in_block_offset + overlap_len]
                        .copy_from_slice(&data[in_data_offset..in_data_offset + overlap_len]);
                }
                buffer[i * 4..i * 4 + 4].copy_from_slice(&block);
            }
        }

        let words: Vec<u32> = buffer
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("chunk length is 4")))
            .collect();

        // Write the entire alignment area at once
        sifli_uart
            .command(SifliUartCommand::MEMWrite {
                addr: start_aligned as u32,
                data: &words,
            })
            .map_err(|e| ArmError::Other(format!("{:?}", e)))?;

        Ok(())
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        let sifli_uart = &mut self.probe.probe;

        if data.is_empty() {
            return Ok(());
        }

        let address = if (address & 0xff000000) == 0x12000000 {
            (address & 0x00ffffff) | 0x62000000
        } else {
            address
        };

        let addr = address as usize;
        let end_addr = addr + data.len();

        let start_aligned = addr - (addr % 4);
        let end_aligned = end_addr.div_ceil(4) * 4;
        let total_bytes = end_aligned - start_aligned;
        let total_words = total_bytes / 4;

        let resp = sifli_uart
            .command(SifliUartCommand::MEMRead {
                addr: start_aligned as u32,
                len: total_words as u16,
            })
            .map_err(|e| ArmError::Other(format!("{:?}", e)))?;

        let buf = match resp {
            SifliUartResponse::MEMRead { data } if data.len() == total_bytes => data,
            _ => return Err(ArmError::Other("MEMRead Error".to_string())),
        };

        // Copy the area of interest data to data
        let offset = addr - start_aligned;
        data.copy_from_slice(&buf[offset..offset + data.len()]);

        Ok(())
    }
}

#[allow(unused)]
impl MemoryInterface<ArmError> for SifliUartMemoryInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        true
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
