mod protocol;

use std::{convert::TryInto, iter};

use crate::{
    architecture::{
        arm::{
            communication_interface::{DapProbe, UninitializedArmProbe},
            SwoAccess,
        },
        riscv::communication_interface::{RiscvCommunicationInterface, RiscvError},
    },
    probe::common::extract_ir_lengths,
    DebugProbe, DebugProbeError, DebugProbeSelector, WireProtocol,
};
use anyhow::anyhow;
use bitvec::prelude::*;
use num_traits::WrappingSub;

use self::protocol::ProtocolHandler;

use super::{ChainParams, JTAGAccess, JtagChainItem};

use probe_rs_target::ScanChainElement;
pub use protocol::list_espjtag_devices;

#[derive(Debug)]
pub(crate) struct EspUsbJtag {
    protocol: ProtocolHandler,

    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    jtag_idle_cycles: u8,

    current_ir_reg: u8,
    max_ir_address: u8,
    scan_chain: Option<Vec<ScanChainElement>>,
    chain_params: Option<ChainParams>,
}

impl EspUsbJtag {
    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    fn scan(&mut self) -> Result<Vec<super::JtagChainItem>, DebugProbeError> {
        let chain = self.reset_scan()?;
        Ok(chain
            .0
            .iter()
            .zip(chain.1.iter())
            .map(|(&id, &ir)| JtagChainItem {
                irlen: ir,
                idcode: id,
            })
            .collect())
    }

    fn reset_scan(&mut self) -> Result<(Vec<u32>, Vec<usize>), super::DebugProbeError> {
        let max_chain = 8;

        tracing::debug!("Resetting JTAG chain using trst");
        // TODO this isn't actually needed, we should only do this when AttachUnderReset it supplied
        self.protocol.set_reset(true, true)?;
        self.protocol.set_reset(false, false)?;

        self.jtag_reset()?;

        let input = Vec::from_iter(iter::repeat(0xFFu8).take(4 * max_chain));
        let response = self.write_dr(&input, 4 * max_chain * 8).unwrap();

        tracing::trace!("DR: {:?}", response);

        let mut idcodes = Vec::new();

        for idcode in response.chunks(4) {
            if idcode.len() != 4 {
                panic!("Bad length");
            }
            if idcode == [0xFF, 0xFF, 0xFF, 0xFF] {
                break;
            }
            idcodes.push(u32::from_le_bytes((idcode).try_into().unwrap()));
        }

        tracing::info!(
            "JTAG dr scan complete, found {} TAPS. {:?}",
            idcodes.len(),
            idcodes
        );

        let input = Vec::from_iter(iter::repeat(0xffu8).take(idcodes.len()));
        let mut response = self.write_ir(&input, idcodes.len() * 8).unwrap();

        let expected = if let Some(ref chain) = self.scan_chain {
            let expected = chain
                .iter()
                .filter_map(|s| s.ir_len)
                .map(|s| s as usize)
                .collect::<Vec<usize>>();
            response.truncate(expected.iter().sum());
            Some(expected)
        } else {
            None
        };

        tracing::trace!("ir scan: {}", response.as_bitslice());

        let ir_lens =
            extract_ir_lengths(response.as_bitslice(), idcodes.len(), expected.as_deref()).unwrap();
        tracing::trace!("Detected IR lens: {:?}", ir_lens);

        Ok((idcodes, ir_lens))
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the data
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(&mut self, data: &[u8], len: usize) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        let tms_enter_ir_shift = [true, true, false, false];

        self.prepare_write_ir(data, len)?;
        let mut response = self.protocol.flush()?;
        tracing::trace!("Response: {:?}", response);

        let mut response = response.split_off(tms_enter_ir_shift.len());
        if let Some(ref params) = self.chain_params {
            response = response.split_off(params.irpre); // cut the prepended bypass commands
        }
        response.truncate(len); // cut the post pended bypass commands

        Ok(response)
    }

    fn prepare_write_ir(&mut self, data: &[u8], len: usize) -> Result<usize, DebugProbeError> {
        tracing::debug!("Write IR: {:?}, len={}", data, len);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < len || len == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        let tms_enter_ir_shift = bitvec![1, 1, 0, 0];

        // The last bit will be transmitted when exiting the shift state,
        // so we need to stay in the shift state for one period less than
        // we have bits to transmit.
        let tms_data = iter::repeat(false).take(len - 1);

        let tms_enter_idle = bitvec![1, 1, 0];

        let mut tms: BitVec<u8, Lsb0> =
            BitVec::with_capacity(tms_enter_ir_shift.len() + len + tms_enter_idle.len());

        tms.extend_from_bitslice(&tms_enter_ir_shift);
        if let Some(ref params) = self.chain_params {
            tms.extend(iter::repeat(false).take(params.irpre))
        }
        tms.extend(tms_data);
        if let Some(ref params) = self.chain_params {
            tms.extend(iter::repeat(false).take(params.irpost))
        }
        tms.extend_from_bitslice(&tms_enter_idle);

        let tdi_enter_ir_shift = bitvec![0, 0, 0, 0];

        // This is one less than the enter idle for tms, because
        // the last bit is transmitted when exiting the IR shift state
        let tdi_enter_idle = bitvec![0, 0];

        let mut tdi: BitVec<u8, Lsb0> =
            BitVec::with_capacity(tdi_enter_ir_shift.len() + len + tdi_enter_idle.len());

        tdi.extend_from_bitslice(&tdi_enter_ir_shift);

        // Add BYPASS commands before shifting out data where required
        if let Some(ref params) = self.chain_params {
            tdi.extend(iter::repeat(true).take(params.irpre))
        }

        let bs = &data.as_bits::<Lsb0>()[..len];
        tdi.extend_from_bitslice(bs);

        // Add BYPASS commands after shifting out data
        if let Some(ref params) = self.chain_params {
            tdi.extend(iter::repeat(true).take(params.irpost))
        }

        tdi.extend_from_bitslice(&tdi_enter_idle);

        tracing::trace!("tms: {:?}", tms);
        tracing::trace!("tdi: {:?}", tdi);

        let len = tms.len();
        self.protocol.jtag_io_async(tms, tdi, true)?;

        Ok(len)
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        self.prepare_write_dr(data, register_bits)?;
        let response = self.protocol.flush()?;
        self.recieve_write_dr(response, register_bits)
    }

    fn recieve_write_dr(
        &mut self,
        mut response: BitVec<u8, Lsb0>,
        register_bits: usize,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let mut response = response.split_off(3); // split off tms_enter_shift from the response

        if let Some(ref params) = self.chain_params {
            response = response.split_off(params.drpre); // cut the prepended bypass command dummy bits
        }

        response.truncate(register_bits);
        response.force_align();
        let result = response.into_vec();
        tracing::trace!("recieve_write_dr result: {:?}", result);
        Ok(result)
    }

    fn prepare_write_dr(
        &mut self,
        data: &[u8],
        register_bits: usize,
    ) -> Result<usize, DebugProbeError> {
        tracing::debug!("Write DR: {:?}, len={}", data, register_bits);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < register_bits || register_bits == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        let tms_enter_shift = bitvec![1, 0, 0];

        // Last bit of data is shifted out when we exi the SHIFT-DR State
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        let tms_enter_idle = bitvec![1, 1, 0];

        let mut tms: BitVec<u8, Lsb0> = BitVec::with_capacity(register_bits + 7);

        tms.extend_from_bitslice(&tms_enter_shift);
        if let Some(ref params) = self.chain_params {
            tms.extend(iter::repeat(false).take(params.drpre))
        }
        tms.extend(tms_shift_out_value);
        if let Some(ref params) = self.chain_params {
            tms.extend(iter::repeat(false).take(params.drpost))
        }
        tms.extend_from_bitslice(&tms_enter_idle);

        let tdi_enter_shift = bitvec![0, 0, 0];

        let tdi_enter_idle = bitvec![0, 0];

        let mut tdi: BitVec<u8, Lsb0> =
            BitVec::with_capacity(tdi_enter_shift.len() + tdi_enter_idle.len() + register_bits);

        tdi.extend_from_bitslice(&tdi_enter_shift);

        // dummy bits to account for bypasses
        if let Some(ref params) = self.chain_params {
            tdi.extend(iter::repeat(true).take(params.drpre))
        }

        let bs = &data.as_bits::<Lsb0>()[..register_bits];
        tdi.extend_from_bitslice(bs);

        // dummy bits to account for bypasses
        if let Some(ref params) = self.chain_params {
            tdi.extend(iter::repeat(true).take(params.drpost))
        }

        tdi.extend_from_bitslice(&tdi_enter_idle);

        // We need to stay in the idle cycle a bit
        tms.extend(iter::repeat(false).take(self.idle_cycles() as usize));
        tdi.extend(iter::repeat(false).take(self.idle_cycles() as usize));

        let len = tms.len();
        self.protocol.jtag_io_async(tms, tdi, true)?;

        Ok(len)
    }

    /// Write the data register
    fn prepare_write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<DeferredRegisterWrite, DebugProbeError> {
        if address > self.max_ir_address.into() {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }
        let address = address.to_le_bytes()[0];

        let write_ir_bits = if self.current_ir_reg != address {
            // Write IR register
            let def = self.prepare_write_ir(&[address], 5)?;
            self.current_ir_reg = address;
            Some(def)
        } else {
            None
        };

        // write DR register
        let write_dr_bits_total = self.prepare_write_dr(data, len as usize)?;

        Ok(DeferredRegisterWrite {
            write_ir_bits,
            write_dr_bits_total,
            write_dr_bits: len as usize,
        })
    }

    fn jtag_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = vec![true, true, true, true, true, false];
        let tdi = iter::repeat(true).take(6);

        let response = self.protocol.jtag_io(tms, tdi, true)?;

        tracing::debug!("Response to reset: {}", response);

        Ok(())
    }
}

pub struct DeferredRegisterWrite {
    write_ir_bits: Option<usize>,
    write_dr_bits_total: usize,
    write_dr_bits: usize,
}

impl JTAGAccess for EspUsbJtag {
    fn set_ir_len(&mut self, len: u32) {
        if len != 5 {
            panic!("Only IR Length of 5 is currently supported");
        }
    }

    /// Read the data register
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        if address > self.max_ir_address.into() {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }
        let address = address.to_le_bytes()[0];

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&[address], 5)?;
            self.current_ir_reg = address;
        }

        // read DR register by transfering len bits to the chain
        let data: Vec<u8> = iter::repeat(0).take((len as usize + 7) / 8).collect();
        self.write_dr(&data, len as usize)
    }

    /// Write the data register
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        if address > self.max_ir_address.into() {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }
        let address = address.to_le_bytes()[0];

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&[address], 5)?;
            self.current_ir_reg = address;
        }

        // write DR register
        self.write_dr(data, len as usize)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        self.jtag_idle_cycles = idle_cycles;
    }

    fn get_idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    fn write_register_batch(
        &mut self,
        writes: &[super::JtagWriteCommand],
    ) -> Result<Vec<super::CommandResult>, super::BatchExecutionError> {
        let mut bits = Vec::with_capacity(writes.len());
        let t1 = std::time::Instant::now();
        tracing::debug!("Preparing {} writes...", writes.len());
        for write in writes {
            bits.push(
                // If an error happens during prep, return no results as chip will be in an inconsistent state
                self.prepare_write_register(write.address, &write.data, write.len)
                    .map_err(|e| super::BatchExecutionError::new(e.into(), Vec::new()))?,
            );
        }

        tracing::debug!("Sending to chip...");
        // If an error happens during the final flush, also retry whole operation
        let mut response = self
            .protocol
            .flush()
            .map_err(|e| super::BatchExecutionError::new(e.into(), Vec::new()))?;
        tracing::debug!("Got responses! Took {:?}! Processing...", t1.elapsed());
        let mut responses = Vec::with_capacity(bits.len());

        for (index, bit) in bits.into_iter().enumerate() {
            if let Some(ir_bits) = bit.write_ir_bits {
                response = response.split_off(ir_bits);
            }
            let split = response.split_off(bit.write_dr_bits_total);
            let v = self
                .recieve_write_dr(response, bit.write_dr_bits)
                .map_err(|e| super::BatchExecutionError::new(e.into(), responses.clone()))?;
            response = split;

            let transform = writes[index].transform;
            let t =
                transform(v).map_err(|e| super::BatchExecutionError::new(e, responses.clone()))?;
            responses.push(t);
        }

        Ok(responses)
    }
}

impl DebugProbe for EspUsbJtag {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError> {
        let protocol = ProtocolHandler::new_from_selector(selector)?;

        Ok(Box::new(EspUsbJtag {
            protocol,
            jtag_idle_cycles: 0,
            current_ir_reg: 1,
            // default to 5, as most Espressif chips have an irlen of 5
            max_ir_address: 5,
            scan_chain: None,
            chain_params: None,
        }))
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if matches!(protocol, WireProtocol::Jtag) {
            Ok(())
        } else {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Jtag)
    }

    fn get_name(&self) -> &'static str {
        "Esp USB JTAG"
    }

    fn speed_khz(&self) -> u32 {
        self.protocol.base_speed_khz / self.protocol.div_min as u32
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        // TODO:
        // can only go lower, base speed it max of 40000khz

        Ok(speed_khz)
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        tracing::info!("Setting scan chain to {:?}", scan_chain);
        self.scan_chain = Some(scan_chain);
        Ok(())
    }

    fn attach(&mut self) -> Result<(), super::DebugProbeError> {
        tracing::debug!("Attaching to ESP USB JTAG");

        let taps = self.scan()?;
        tracing::info!("Found {} taps on reset scan", taps.len());

        let selected = 0;
        if taps.len() > 1 {
            tracing::warn!("More than on tap detected, defaulting to tap0")
        }

        let mut params = ChainParams {
            irpre: 0,
            irpost: 0,
            drpre: 0,
            drpost: 0,
            irlen: 0,
        };

        let mut found = false;
        for (index, tap) in taps.iter().enumerate() {
            tracing::info!("{:?}", tap);
            if index == selected {
                params.irlen = tap.irlen;
                found = true;
            } else if found {
                params.irpost += tap.irlen;
                params.drpost += 1;
            } else {
                params.irpre += tap.irlen;
                params.drpre += 1;
            }
        }

        tracing::info!("Setting chain params: {:?}", params);

        // set the max address to the max number of bits irlen can represent
        self.max_ir_address = ((1 << params.irlen).wrapping_sub(&1)) as u8;
        tracing::debug!("Setting max_ir_address to {}", self.max_ir_address);
        self.chain_params = Some(params);

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), super::DebugProbeError> {
        Err(super::DebugProbeError::NotImplemented("target_reset"))
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("reset_assert!");
        self.protocol.set_reset(false, false)?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("reset_deassert!");
        self.protocol.set_reset(true, true)?;
        Ok(())
    }

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        // This probe is intended for RISC-V.
        match RiscvCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        // This probe cannot debug ARM targets.
        None
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        // This probe cannot debug ARM targets.
        None
    }

    fn has_arm_interface(&self) -> bool {
        // This probe cannot debug ARM targets.
        false
    }

    fn has_riscv_interface(&self) -> bool {
        // This probe is intended for RISC-V.
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        // This is not a DAP capable probe.
        None
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        // This probe cannot debug ARM targets.
        Err((self, DebugProbeError::InterfaceNotAvailable("SWD/ARM")))
    }

    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        // We cannot read the voltage on this probe, unfortunately.
        Ok(None)
    }
}
