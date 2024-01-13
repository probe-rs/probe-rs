use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::architecture::{
    arm::communication_interface::UninitializedArmProbe,
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::probe::common::{JtagState, RegisterState};
use crate::probe::{BatchExecutionError, DeferredResultSet, JtagCommandQueue};
use crate::{
    probe::{
        common::{common_sequence, extract_idcodes, extract_ir_lengths},
        DebugProbe, JTAGAccess, ProbeCreationError, ProbeDriver, ScanChainElement,
    },
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, WireProtocol,
};
use anyhow::anyhow;
use bitvec::prelude::*;
use nusb::DeviceInfo;
use std::io::{Read, Write};
use std::iter;
use std::time::Duration;

mod ftdi_impl;
use ftdi_impl as ftdi;

use super::{ChainParams, JtagChainItem};

#[derive(Clone, Debug)]
enum Command {
    None {
        tms: bool,
    },
    TmsBits {
        bit_count: usize,
        tms_bits: u8,
        tdi: bool,
        capture: bool,
    },
    TdiBits {
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },
    TdiSequence {
        tdi_bytes: Vec<u8>,
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },
}

impl Default for Command {
    fn default() -> Self {
        Self::None { tms: false }
    }
}

impl Command {
    fn update(&mut self, tms: bool, tdi: bool, capture: bool) -> Option<Self> {
        match self {
            Self::None { tms: tms_prev } => {
                *self = if !tms && !*tms_prev {
                    Self::TdiBits {
                        bit_count: 1,
                        tdi_bits: tdi as u8,
                        capture,
                    }
                } else {
                    Self::TmsBits {
                        bit_count: 1,
                        tms_bits: tms as u8,
                        tdi,
                        capture,
                    }
                };

                None
            }

            Self::TmsBits {
                bit_count,
                tms_bits,
                tdi: tdi_prev,
                capture: capture_prev,
            } => {
                let same_tdi = *tdi_prev == tdi;
                let same_capture = *capture_prev == capture;
                let tms_prev = *tms_bits & (0x01 << (*bit_count - 1)) != 0;

                if same_tdi && same_capture {
                    *tms_bits |= (tms as u8) << *bit_count;
                    *bit_count += 1;

                    if *bit_count == 6 {
                        Some(self.take())
                    } else {
                        None
                    }
                } else {
                    let mut new = Self::None { tms: tms_prev };
                    new.update(tms, tdi, capture);
                    Some(std::mem::replace(self, new))
                }
            }

            Self::TdiBits {
                bit_count,
                tdi_bits,
                capture: capture_prev,
            } => {
                let same_tms = !tms;
                let same_capture = *capture_prev == capture;

                if same_tms && same_capture {
                    *tdi_bits |= (tdi as u8) << *bit_count;
                    *bit_count += 1;

                    if *bit_count == 8 {
                        *self = Self::TdiSequence {
                            tdi_bytes: vec![*tdi_bits],
                            bit_count: 0,
                            tdi_bits: 0,
                            capture,
                        };
                    }
                    None
                } else {
                    let mut new = Self::None { tms: false };
                    new.update(tms, tdi, capture);
                    Some(std::mem::replace(self, new))
                }
            }

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                tdi_bits,
                capture: capture_prev,
            } => {
                let same_tms = !tms;
                let same_capture = *capture_prev == capture;

                if same_tms && same_capture {
                    *tdi_bits |= (tdi as u8) << *bit_count;
                    *bit_count += 1;

                    if *bit_count == 8 {
                        tdi_bytes.push(*tdi_bits);
                        *bit_count = 0;
                        *tdi_bits = 0;
                    }

                    // Split early, but make sure we won't split inside a command
                    if tdi_bytes.len() == 16380 {
                        Some(self.take())
                    } else {
                        None
                    }
                } else {
                    let mut new = Self::None { tms: false };
                    new.update(tms, tdi, capture);
                    Some(std::mem::replace(self, new))
                }
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::None { .. } => 0,
            Self::TmsBits { .. } | Self::TdiBits { .. } => 3,
            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } => tdi_bytes.len() + 3 + if *bit_count > 0 { 3 } else { 0 },
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::None { .. } => {}
            Self::TmsBits {
                tms_bits,
                tdi,
                capture,
                bit_count,
            } => {
                let tms_byte = tms_bits | ((*tdi as u8) << 7);
                let cap_bit = if *capture { 0x20 } else { 0 };

                out.extend_from_slice(&[0x4b | cap_bit, *bit_count as u8 - 1, tms_byte]);
            }
            Self::TdiBits {
                tdi_bits,
                capture,
                bit_count,
                ..
            } => {
                let cap_bit = if *capture { 0x20 } else { 0 };

                let mut tdi_bits = *tdi_bits;
                let mut bit_count = *bit_count as u8;

                if bit_count >= 7 {
                    // Some FTDI chips have trouble with 7 bits
                    // output 6 bits
                    out.extend_from_slice(&[0x1b | cap_bit, 5, tdi_bits]);

                    tdi_bits >>= 6;
                    bit_count -= 6;
                }
                out.extend_from_slice(&[0x1b | cap_bit, bit_count - 1, tdi_bits]);
            }
            Self::TdiSequence {
                tdi_bytes,
                tdi_bits,
                capture,
                bit_count,
                ..
            } => {
                let cap_bit = if *capture { 0x20 } else { 0 };

                // Append full bytes
                let [n_low, n_high] = (tdi_bytes.len() as u16 - 1).to_le_bytes();
                out.extend_from_slice(&[0x19 | cap_bit, n_low, n_high]);
                out.extend_from_slice(tdi_bytes);

                // Append remaining bits
                let mut tdi_bits = *tdi_bits;
                let mut bit_count = *bit_count as u8;

                if bit_count > 0 {
                    if bit_count >= 7 {
                        // Some FTDI chips have trouble with 7 bits
                        // output 6 bits
                        out.extend_from_slice(&[0x1b | cap_bit, 5, tdi_bits]);

                        tdi_bits >>= 6;
                        bit_count -= 6;
                    }

                    out.extend_from_slice(&[0x1b | cap_bit, bit_count - 1, tdi_bits]);
                }
            }
        }
    }

    fn take(&mut self) -> Self {
        let this = std::mem::take(self);

        let tms = match &this {
            Self::None { tms } => *tms,
            Self::TmsBits {
                tms_bits,
                bit_count,
                ..
            } => (*tms_bits & (0x01 << (*bit_count - 1))) != 0,
            Self::TdiBits { .. } => false,
            Self::TdiSequence { .. } => false,
        };

        *self = Self::None { tms };

        this
    }

    fn add_captured_bits(&self, bits: &mut Vec<usize>) {
        let capture = match self {
            Self::None { .. } => false,
            Self::TmsBits { capture, .. } | Self::TdiBits { capture, .. } => *capture,
            Self::TdiSequence { capture, .. } => *capture,
        };

        if !capture {
            return;
        }

        match self {
            Self::None { .. } => {}
            Self::TmsBits { bit_count, .. } => bits.push(*bit_count),
            Self::TdiBits { bit_count, .. } => {
                if *bit_count == 7 {
                    bits.push(6);
                    bits.push(1);
                } else {
                    bits.push(*bit_count);
                }
            }
            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } => {
                for _ in 0..tdi_bytes.len() {
                    bits.push(8);
                }

                if *bit_count == 7 {
                    bits.push(6);
                    bits.push(1);
                } else if *bit_count > 0 {
                    bits.push(*bit_count);
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct JtagAdapter {
    device: ftdi::Device,
    chain_params: ChainParams,
    jtag_idle_cycles: u8,

    current_ir_reg: u32,
    max_ir_address: u32,

    jtag_state: JtagState,

    command: Command,
    commands: Vec<u8>,
    in_bit_counts: Vec<usize>,
    in_bits: BitVec<u8, Lsb0>,

    scan_chain: Option<Vec<ScanChainElement>>,
}

impl JtagAdapter {
    pub fn open(vid: u16, pid: u16) -> Result<Self, ftdi::Error> {
        let mut builder = ftdi::Builder::new();
        builder.set_interface(ftdi::Interface::A)?;
        let device = builder.usb_open(vid, pid)?;

        Ok(Self {
            device,
            chain_params: ChainParams::default(),
            jtag_idle_cycles: 0,
            jtag_state: JtagState::Reset,
            current_ir_reg: 1,
            max_ir_address: 0x1F,
            command: Command::default(),
            commands: Vec::with_capacity(16384),
            in_bit_counts: vec![],
            in_bits: BitVec::with_capacity(16384),
            scan_chain: None,
        })
    }

    pub fn attach(&mut self) -> Result<(), ftdi::Error> {
        self.device.usb_reset()?;
        self.device.set_latency_timer(1)?;
        self.device.set_bitmode(0x0b, ftdi::BitMode::Mpsse)?;
        self.device.usb_purge_buffers()?;

        let mut junk = vec![];
        let _ = self.device.read_to_end(&mut junk);

        // Minimal values, may not work with all probes
        let output: u16 = 0x0008;
        let direction: u16 = 0x000b;
        self.device
            .write_all(&[0x80, output as u8, direction as u8])?;
        self.device
            .write_all(&[0x82, (output >> 8) as u8, (direction >> 8) as u8])?;

        // Disable loopback
        self.device.write_all(&[0x85])?;

        Ok(())
    }

    fn read_response(&mut self) -> Result<(), DebugProbeError> {
        if self.in_bit_counts.is_empty() {
            return Ok(());
        }

        let mut t0 = std::time::Instant::now();
        let timeout = Duration::from_millis(10);

        let mut reply = Vec::with_capacity(self.in_bit_counts.len());
        while reply.len() < self.in_bit_counts.len() {
            if t0.elapsed() > timeout {
                tracing::warn!(
                    "Read {} bytes, expected {}",
                    reply.len(),
                    self.in_bit_counts.len()
                );
                return Err(DebugProbeError::Timeout);
            }

            let read = self
                .device
                .read_to_end(&mut reply)
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

            if read > 0 {
                t0 = std::time::Instant::now();
            }
        }

        if reply.len() != self.in_bit_counts.len() {
            return Err(DebugProbeError::Other(anyhow!(
                "Read more data than expected. Expected {} bytes, got {} bytes",
                self.in_bit_counts.len(),
                reply.len()
            )));
        }

        for (byte, count) in reply.into_iter().zip(self.in_bit_counts.drain(..)) {
            let bits = byte >> (8 - count);
            self.in_bits
                .extend_from_bitslice(&bits.view_bits::<Lsb0>()[..count]);
        }

        Ok(())
    }

    /// Reset and go to RUN-TEST/IDLE
    pub fn reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = [true, true, true, true, true, false];
        let tdi = iter::repeat(false);

        let response = self.jtag_scan(tms, tdi, iter::repeat(true))?;

        tracing::debug!("Response to reset: {:?}", response);

        Ok(())
    }

    fn jtag_move_to_state(&mut self, target: JtagState) -> Result<(), DebugProbeError> {
        tracing::trace!("Changing state: {:?} -> {:?}", self.jtag_state, target);
        while let Some(tms) = self.jtag_state.step_toward(target) {
            self.schedule_jtag_scan([tms], [false], [false])?;
        }
        tracing::trace!("In state: {:?}", self.jtag_state);
        Ok(())
    }

    fn append_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        tracing::debug!("Appending {:?}", command);
        // Max MPSSE buffer size is supposed to be 65536 bytes but due to some limitation 16K works
        // better for me.
        // 1 byte is reserved for the send immediate command
        if self.commands.len() + command.len() + 1 >= 16384 {
            self.send_buffer()?;
            self.read_response()
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        }

        command.add_captured_bits(&mut self.in_bit_counts);
        command.encode(&mut self.commands);

        Ok(())
    }

    fn finalize_command(&mut self) -> Result<(), DebugProbeError> {
        let command = self.command.take();
        self.append_command(command)
    }

    fn schedule_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        if let Some(command) = self.command.update(tms, tdi, capture) {
            self.append_command(command)?;
        }

        Ok(())
    }

    fn schedule_jtag_scan(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: impl IntoIterator<Item = bool>,
    ) -> Result<(), DebugProbeError> {
        for ((tms, tdi), cap) in tms.into_iter().zip(tdi.into_iter()).zip(cap.into_iter()) {
            self.schedule_bit(tms, tdi, cap)?;
            self.jtag_state.update(tms);
        }

        Ok(())
    }

    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        // Send Immediate: This will make the FTDI chip flush its buffer back to the PC.
        // See https://www.ftdichip.com/Support/Documents/AppNotes/AN_108_Command_Processor_for_MPSSE_and_MCU_Host_Bus_Emulation_Modes.pdf
        // section 5.1
        self.commands.push(0x87);

        tracing::trace!("Sending buffer: {:X?}", self.commands);

        self.device
            .write_all(&self.commands)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        self.commands.clear();

        Ok(())
    }

    fn flush(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.finalize_command()?;
        if !self.commands.is_empty() {
            self.send_buffer()?;
        }

        if !self.in_bit_counts.is_empty() {
            self.read_response()
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        }

        Ok(std::mem::take(&mut self.in_bits))
    }

    fn jtag_scan(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        capture: impl IntoIterator<Item = bool>,
    ) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.schedule_jtag_scan(tms, tdi, capture)?;
        self.flush()
    }

    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the data
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn scan_ir(
        &mut self,
        data: &[u8],
        len: usize,
        capture_response: bool,
    ) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.schedule_ir_scan(data, len, capture_response)?;
        let response = self.flush()?;
        tracing::trace!("Response: {:?}", response);

        Ok(response)
    }

    fn schedule_ir_scan(
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
        self.jtag_move_to_state(JtagState::Ir(RegisterState::Shift))?;

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

        self.schedule_jtag_scan(tms, tdi, capture)?;

        self.jtag_move_to_state(JtagState::Ir(RegisterState::Update))?;

        Ok(())
    }

    fn scan_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        self.schedule_dr_scan(data, register_bits, true)?;
        let response = self.flush()?;
        self.recieve_dr_scan(response)
    }

    fn recieve_dr_scan(
        &mut self,
        mut response: BitVec<u8, Lsb0>,
    ) -> Result<Vec<u8>, DebugProbeError> {
        response.force_align();
        let result = response.into_vec();
        tracing::trace!("recieve_write_dr result: {:?}", result);
        Ok(result)
    }

    fn schedule_dr_scan(
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
        self.jtag_move_to_state(JtagState::Dr(RegisterState::Shift))?;

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

        self.schedule_jtag_scan(tms, tdi, capture)?;

        self.jtag_move_to_state(JtagState::Dr(RegisterState::Update))?;

        if self.idle_cycles() > 0 {
            self.jtag_move_to_state(JtagState::Idle)?;

            // We need to stay in the idle cycle a bit
            let tms = iter::repeat(false).take(self.idle_cycles() as usize);
            let tdi = iter::repeat(false).take(self.idle_cycles() as usize);

            self.schedule_jtag_scan(tms, tdi, iter::repeat(false))?;
        }

        if capture_data {
            Ok(register_bits)
        } else {
            Ok(0)
        }
    }

    fn scan(&mut self) -> Result<Vec<JtagChainItem>, DebugProbeError> {
        const MAX_CHAIN: usize = 8;

        self.reset()?;

        self.chain_params = ChainParams::default();

        let input = vec![0xFF; 4 * MAX_CHAIN];
        let response = self.scan_dr(&input, input.len() * 8)?;

        tracing::debug!("DR: {:?}", response);

        let idcodes = extract_idcodes(BitSlice::<u8, Lsb0>::from_slice(&response))
            .map_err(|e| DebugProbeError::Other(e.into()))?;

        tracing::info!(
            "JTAG DR scan complete, found {} TAPs. {:?}",
            idcodes.len(),
            idcodes
        );

        self.reset()?;

        // First shift out all ones
        let input = vec![0xff; idcodes.len()];
        let response = self.scan_ir(&input, input.len() * 8, true)?;

        tracing::debug!("IR scan: {}", response);

        self.reset()?;

        // Next, shift out same amount of zeros, then ones to make sure the IRs contain BYPASS.
        let input = iter::repeat(0)
            .take(idcodes.len())
            .chain(input.iter().copied())
            .collect::<Vec<_>>();
        let response_zeros = self.scan_ir(&input, input.len() * 8, true)?;

        tracing::debug!("IR scan: {}", response_zeros);

        let expected = if let Some(ref chain) = self.scan_chain {
            let expected = chain
                .iter()
                .filter_map(|s| s.ir_len)
                .map(|s| s as usize)
                .collect::<Vec<usize>>();
            Some(expected)
        } else {
            None
        };

        let response = response.as_bitslice();
        let response = common_sequence(response, response_zeros.as_bitslice());

        tracing::debug!("IR scan: {}", response);

        let ir_lens = extract_ir_lengths(response, idcodes.len(), expected.as_deref())
            .map_err(|e| DebugProbeError::Other(e.into()))?;
        tracing::debug!("Detected IR lens: {:?}", ir_lens);

        Ok(idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| JtagChainItem { irlen, idcode })
            .collect())
    }

    fn select_target(
        &mut self,
        chain: &[JtagChainItem],
        selected: usize,
    ) -> Result<(), DebugProbeError> {
        let Some(params) = ChainParams::from_jtag_chain(chain, selected) else {
            return Err(DebugProbeError::TargetNotFound);
        };

        tracing::debug!("Target chain params: {:?}", params);
        self.chain_params = params;

        self.max_ir_address = (1 << params.irlen) - 1;
        tracing::debug!("Setting max_ir_address to {}", self.max_ir_address);

        Ok(())
    }

    /// Write the data register
    fn prepare_write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
        capture_data: bool,
    ) -> Result<DeferredRegisterWrite, DebugProbeError> {
        if address > self.max_ir_address {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }
        let address_bytes = address.to_le_bytes();

        if self.current_ir_reg != address {
            // Write IR register
            self.schedule_ir_scan(&address_bytes, self.chain_params.irlen, false)?;
            self.current_ir_reg = address;
        }

        // write DR register
        let len = self.schedule_dr_scan(data, len as usize, capture_data)?;

        Ok(DeferredRegisterWrite { len })
    }
}

struct DeferredRegisterWrite {
    len: usize,
}

pub struct FtdiProbeSource;

impl std::fmt::Debug for FtdiProbeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FTDI").finish()
    }
}

impl ProbeDriver for FtdiProbeSource {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        // Only open FTDI-compatible probes
        if !FTDI_COMPAT_DEVICE_IDS.contains(&(selector.vendor_id, selector.product_id)) {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }

        let adapter = JtagAdapter::open(selector.vendor_id, selector.product_id)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let probe = FtdiProbe {
            adapter,
            speed_khz: 0,
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_ftdi_devices()
    }
}

#[derive(Debug)]
pub struct FtdiProbe {
    adapter: JtagAdapter,
    speed_khz: u32,
}

impl DebugProbe for FtdiProbe {
    fn get_name(&self) -> &str {
        "FTDI"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.speed_khz = speed_khz;
        // TODO
        Ok(speed_khz)
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        tracing::info!("Setting scan chain to {:?}", scan_chain);
        self.adapter.scan_chain = Some(scan_chain);
        Ok(())
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("attaching...");

        self.adapter
            .attach()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let taps = self.adapter.scan()?;
        if taps.is_empty() {
            tracing::warn!("no JTAG taps detected");
            return Err(DebugProbeError::TargetNotFound);
        }
        if taps.len() == 1 {
            self.adapter
                .select_target(&taps, 0)
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        } else {
            const KNOWN_IDCODES: [u32; 2] = [
                0x1000563d, // GD32VF103
                0x120034e5, // Little endian Xtensa core
            ];
            let idcode = taps.iter().map(|tap| tap.idcode).position(|idcode| {
                let Some(idcode) = idcode else {
                    return false;
                };

                let found = KNOWN_IDCODES.contains(&idcode.0);
                if !found {
                    tracing::warn!("Unknown IDCODEs: {:x?}", idcode);
                }
                found
            });
            if let Some(pos) = idcode {
                self.adapter
                    .select_target(&taps, pos)
                    .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
            } else {
                return Err(DebugProbeError::TargetNotFound);
            }
        }
        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset");
        unimplemented!()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset_assert");
        unimplemented!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset_deassert");
        unimplemented!()
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if protocol != WireProtocol::Jtag {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        // Only supports JTAG
        Some(WireProtocol::Jtag)
    }

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        match RiscvCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        todo!()
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

impl JTAGAccess for FtdiProbe {
    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        tracing::debug!("set_idle_cycles({})", idle_cycles);
        self.adapter.jtag_idle_cycles = idle_cycles;
    }

    fn idle_cycles(&self) -> u8 {
        self.adapter.jtag_idle_cycles
    }

    /// Write the data register
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        if address > self.adapter.max_ir_address {
            return Err(DebugProbeError::Other(anyhow!(
                "JTAG Register addresses are fixed to {} bits",
                self.adapter.chain_params.irlen
            )));
        }
        let address_bytes = address.to_le_bytes();

        if self.adapter.current_ir_reg != address {
            // Write IR register
            self.adapter
                .scan_ir(&address_bytes, self.adapter.chain_params.irlen, false)?;
            self.adapter.current_ir_reg = address;
        }

        // write DR register
        self.adapter.scan_dr(data, len as usize)
    }

    fn set_ir_len(&mut self, _len: u32) {
        // The FTDI implementation automatically sets this, so no need to act on this data
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
                .adapter
                .prepare_write_register(write.address, &write.data, write.len, idx.should_capture())
                .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

            bits.push((idx, write.transform, op));
        }

        tracing::debug!("Sending to chip...");
        // If an error happens during the final flush, also retry whole operation
        let bitstream = self
            .adapter
            .flush()
            .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

        tracing::debug!("Got response! Took {:?}! Processing...", t1.elapsed(),);
        let mut responses = DeferredResultSet::with_capacity(bits.len());

        let mut bitstream = bitstream.as_bitslice();
        for (idx, transform, bit) in bits.into_iter() {
            if idx.should_capture() {
                let write_response = match self
                    .adapter
                    .recieve_dr_scan(bitstream[..bit.len].to_bitvec())
                {
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

/// (VendorId, ProductId)
static FTDI_COMPAT_DEVICE_IDS: &[(u16, u16)] = &[
    (0x0403, 0x6010), // FTDI Ltd. FT2232C/D/H Dual UART/FIFO IC
    (0x0403, 0x6011), // FTDI Ltd. FT4232H Quad HS USB-UART/FIFO IC
    (0x0403, 0x6014), // FTDI Ltd. FT232H Single HS USB-UART/FIFO IC
    (0x15ba, 0x002a), // Olimex Ltd. ARM-USB-TINY-H JTAG interface
];

fn get_device_info(device: &DeviceInfo) -> Option<DebugProbeInfo> {
    if !FTDI_COMPAT_DEVICE_IDS.contains(&(device.vendor_id(), device.product_id())) {
        return None;
    }

    Some(DebugProbeInfo {
        identifier: device.product_string().unwrap_or("FTDI").to_string(),
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        serial_number: device.serial_number().map(|s| s.to_string()),
        probe_type: &FtdiProbeSource,
        hid_interface: None,
    })
}

#[tracing::instrument(skip_all)]
fn list_ftdi_devices() -> Vec<DebugProbeInfo> {
    match nusb::list_devices() {
        Ok(devices) => devices
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(_) => vec![],
    }
}
