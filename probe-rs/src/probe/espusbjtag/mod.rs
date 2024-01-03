mod protocol;

use std::{convert::TryInto, iter};

use crate::{
    architecture::{
        arm::{
            communication_interface::{DapProbe, UninitializedArmProbe},
            SwoAccess,
        },
        riscv::communication_interface::{RiscvCommunicationInterface, RiscvError},
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    probe::{
        common::extract_ir_lengths,
        espusbjtag::protocol::{JtagState, RegisterState},
        DeferredResultSet, JtagCommandQueue,
    },
    DebugProbe, DebugProbeError, DebugProbeSelector, WireProtocol,
};
use anyhow::anyhow;
use bitvec::prelude::*;
use num_traits::WrappingSub;

use self::protocol::ProtocolHandler;

use super::{BatchExecutionError, ChainParams, JTAGAccess, JtagChainItem};

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
    chain_params: ChainParams,
}

impl EspUsbJtag {
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

        self.jtag_reset()?;

        self.chain_params = ChainParams {
            irpre: 0,
            irpost: 0,
            drpre: 0,
            drpost: 0,
            irlen: 0,
        };

        let input = Vec::from_iter(iter::repeat(0xFFu8).take(4 * max_chain));
        let response = self.write_dr(&input, 4 * max_chain * 8).unwrap();

        tracing::trace!("DR: {:?}", response);

        let mut idcodes = Vec::new();

        for idcode in response.chunks(4) {
            assert_eq!(idcode.len(), 4, "Bad length");

            if idcode == [0xFF, 0xFF, 0xFF, 0xFF] {
                break;
            }
            idcodes.push(u32::from_le_bytes((idcode).try_into().unwrap()));
        }

        tracing::info!(
            "JTAG DR scan complete, found {} TAPs. {:?}",
            idcodes.len(),
            idcodes
        );

        let input = Vec::from_iter(iter::repeat(0xffu8).take(idcodes.len()));
        let mut response = self.write_ir(&input, idcodes.len() * 8, true).unwrap();

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

        tracing::trace!("IR scan: {}", response.as_bitslice());

        let ir_lens =
            extract_ir_lengths(response.as_bitslice(), idcodes.len(), expected.as_deref()).unwrap();
        tracing::trace!("Detected IR lens: {:?}", ir_lens);

        Ok((idcodes, ir_lens))
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the data
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(
        &mut self,
        data: &[u8],
        len: usize,
        capture_response: bool,
    ) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.prepare_write_ir(data, len, capture_response)?;
        let response = self.protocol.flush()?;
        tracing::trace!("Response: {:?}", response);

        Ok(response)
    }

    fn prepare_write_ir(
        &mut self,
        data: &[u8],
        len: usize,
        capture_data: bool,
    ) -> Result<(), DebugProbeError> {
        tracing::debug!("Write IR: {:?}, len={}", data, len);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < len || len == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        // BYPASS commands before and after shifting out data where required
        let pre_bits = self.chain_params.irpre;
        let post_bits = self.chain_params.irpost;

        // The last bit will be transmitted when exiting the shift state,
        // so we need to stay in the shift state for one period less than
        // we have bits to transmit.
        let tms_data = iter::repeat(false).take(len - 1);

        // Enter IR shift
        self.protocol
            .jtag_move_to_state(JtagState::Ir(RegisterState::Shift))?;

        let tms = iter::repeat(false)
            .take(pre_bits)
            .chain(tms_data)
            .chain(iter::repeat(false).take(post_bits))
            .chain(iter::once(true));

        let tdi = iter::repeat(true)
            .take(pre_bits)
            .chain(data.as_bits::<Lsb0>()[..len].iter().map(|b| *b))
            .chain(iter::repeat(true).take(post_bits));

        let capture = iter::repeat(false)
            .take(pre_bits)
            .chain(iter::repeat(capture_data).take(len))
            .chain(iter::repeat(false));

        tracing::trace!("tms: {:?}", tms.clone());
        tracing::trace!("tdi: {:?}", tdi.clone());

        self.protocol.jtag_io_async2(tms, tdi, capture)?;

        self.protocol
            .jtag_move_to_state(JtagState::Ir(RegisterState::Update))?;

        Ok(())
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        self.prepare_write_dr(data, register_bits, true)?;
        let response = self.protocol.flush()?;
        self.recieve_write_dr(response)
    }

    fn recieve_write_dr(
        &mut self,
        mut response: BitVec<u8, Lsb0>,
    ) -> Result<Vec<u8>, DebugProbeError> {
        response.force_align();
        let result = response.into_vec();
        tracing::trace!("recieve_write_dr result: {:?}", result);
        Ok(result)
    }

    fn prepare_write_dr(
        &mut self,
        data: &[u8],
        register_bits: usize,
        capture_data: bool,
    ) -> Result<usize, DebugProbeError> {
        tracing::debug!("Write DR: {:?}, len={}", data, register_bits);

        // Check the bit length, enough data has to be available
        if data.len() * 8 < register_bits || register_bits == 0 {
            return Err(DebugProbeError::Other(anyhow!("Invalid data length")));
        }

        // Last bit of data is shifted out when we exit the SHIFT-DR State
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        // Enter DR shift
        self.protocol
            .jtag_move_to_state(JtagState::Dr(RegisterState::Shift))?;

        // dummy bits to account for bypasses
        let pre_bits = self.chain_params.drpre;
        let post_bits = self.chain_params.drpost;

        let tms = iter::repeat(false)
            .take(pre_bits)
            .chain(tms_shift_out_value)
            .chain(iter::repeat(false).take(post_bits))
            .chain(iter::once(true));

        let tdi = iter::repeat(false)
            .take(pre_bits)
            .chain(data.as_bits::<Lsb0>()[..register_bits].iter().map(|b| *b))
            .chain(iter::repeat(false).take(post_bits));

        let capture = iter::repeat(false)
            .take(pre_bits)
            .chain(iter::repeat(capture_data).take(register_bits))
            .chain(iter::repeat(false));

        self.protocol.jtag_io_async2(tms, tdi, capture)?;

        self.protocol
            .jtag_move_to_state(JtagState::Dr(RegisterState::Update))?;

        if self.idle_cycles() > 0 {
            self.protocol.jtag_move_to_state(JtagState::Idle)?;

            // We need to stay in the idle cycle a bit
            let tms = iter::repeat(false).take(self.idle_cycles() as usize);
            let tdi = iter::repeat(false).take(self.idle_cycles() as usize);

            self.protocol.jtag_io_async(tms, tdi, false)?;
        }

        if capture_data {
            Ok(register_bits)
        } else {
            Ok(0)
        }
    }

    /// Write the data register
    fn prepare_write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
        capture_data: bool,
    ) -> Result<DeferredRegisterWrite, DebugProbeError> {
        if address > self.max_ir_address.into() {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }
        let address = address.to_le_bytes()[0];

        if self.current_ir_reg != address {
            // Write IR register
            self.prepare_write_ir(&[address], 5, false)?;
            self.current_ir_reg = address;
        }

        // write DR register
        let len = self.prepare_write_dr(data, len as usize, capture_data)?;

        Ok(DeferredRegisterWrite { len })
    }

    fn jtag_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = [true, true, true, true, true, false];
        let tdi = iter::repeat(true);

        let response = self.protocol.jtag_io(tms, tdi, true)?;

        tracing::debug!("Response to reset: {}", response);

        Ok(())
    }
}

pub struct DeferredRegisterWrite {
    len: usize,
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
            self.write_ir(&[address], 5, false)?;
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
            self.write_ir(&[address], 5, false)?;
            self.current_ir_reg = address;
        }

        // write DR register
        self.write_dr(data, len as usize)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        self.jtag_idle_cycles = idle_cycles;
    }

    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    fn write_register_batch(
        &mut self,
        writes: &JtagCommandQueue,
    ) -> Result<DeferredResultSet, BatchExecutionError> {
        let mut bits = Vec::with_capacity(writes.len());
        let t1 = std::time::Instant::now();
        tracing::debug!("Preparing {} writes...", writes.len());
        for (idx, write) in writes.iter() {
            // If an error happens during prep, return no results as chip will be in an inconsistent state
            let op = self
                .prepare_write_register(write.address, &write.data, write.len, idx.should_capture())
                .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

            bits.push((idx, write.transform, op));
        }

        tracing::debug!("Sending to chip...");
        // If an error happens during the final flush, also retry whole operation
        let bitstream = self
            .protocol
            .flush()
            .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

        tracing::debug!("Got responses! Took {:?}! Processing...", t1.elapsed());
        let mut responses = DeferredResultSet::with_capacity(bits.len());

        let mut bitstream = bitstream.as_bitslice();
        for (idx, transform, bit) in bits.into_iter() {
            if idx.should_capture() {
                let write_response = match self.recieve_write_dr(bitstream[..bit.len].to_bitvec()) {
                    Ok(response_bits) => transform(response_bits),
                    Err(e) => Err(e.into()),
                };

                match write_response {
                    Ok(response) => responses.push(idx, response),
                    Err(e) => return Err(BatchExecutionError::new(e, responses)),
                }
            }

            bitstream = &bitstream[bit.len..];
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
            chain_params: ChainParams {
                irpre: 0,
                irpost: 0,
                drpre: 0,
                drpost: 0,
                irlen: 0,
            },
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
        // can only go lower, base speed is max of 40000khz

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
        tracing::info!("Found {} TAPs on reset scan", taps.len());

        let selected = 0;
        if taps.len() > 1 {
            tracing::warn!("More than one TAP detected, defaulting to tap0")
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
        self.chain_params = params;

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
        self.protocol.set_reset(true)?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("reset_deassert!");
        self.protocol.set_reset(false)?;
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

    fn try_get_xtensa_interface(
        self: Box<Self>,
    ) -> Result<XtensaCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        // This probe is intended for Xtensa.
        match XtensaCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }
}
