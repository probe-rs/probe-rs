mod protocol;

use std::{
    convert::TryInto,
    iter,
    time::{Duration, Instant},
};

use crate::{
    architecture::{
        arm::{
            communication_interface::{DapProbe, UninitializedArmProbe},
            SwoAccess,
        },
        riscv::communication_interface::{RiscvCommunicationInterface, RiscvError},
    },
    probe::jlink::bits_to_byte,
    DebugProbe, DebugProbeError, DebugProbeSelector, WireProtocol,
};

use self::protocol::{BitIter, ProtocolHandler};

use super::JTAGAccess;

pub use protocol::list_espjtag_devices;

#[derive(Debug)]
pub(crate) struct EspUsbJtag {
    protocol: ProtocolHandler,

    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    jtag_idle_cycles: u8,

    current_ir_reg: u32,
}

impl EspUsbJtag {
    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    fn read_dr(&mut self, register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("Read {} bits from DR", register_bits);

        let tms_enter_shift = [true, false, false];

        // Last bit of data is shifted out when we exit the SHIFT-DR State.
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        let tms_enter_idle = [true, true, false];

        let mut tms = Vec::with_capacity(register_bits + 7);

        tms.extend_from_slice(&tms_enter_shift);
        tms.extend(tms_shift_out_value);
        tms.extend_from_slice(&tms_enter_idle);

        let tdi = iter::repeat(false).take(tms.len() + self.idle_cycles() as usize);

        // We have to stay in the idle cycle a bit
        tms.extend(iter::repeat(false).take(self.idle_cycles() as usize));

        let mut response = self.protocol.jtag_io(tms, tdi, true)?;
        let mut response = response.iter();

        tracing::trace!("Response: {:?}", response);

        let _remainder = response.split_off(tms_enter_shift.len());

        let mut remaining_bits = register_bits;

        let mut result = Vec::new();

        while remaining_bits >= 8 {
            let byte = bits_to_byte(response.split_off(8)) as u8;
            result.push(byte);
            remaining_bits -= 8;
        }

        // Handle leftover bytes
        if remaining_bits > 0 {
            result.push(bits_to_byte(response.split_off(remaining_bits)) as u8);
        }

        tracing::debug!("Read from DR: {:?}", result);

        Ok(result)
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the data
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(&mut self, data: &[u8], len: usize) -> Result<(), DebugProbeError> {
        if len >= 8 {
            return Err(DebugProbeError::NotImplemented(
                "Not yet implemented for IR registers larger than 8 bit",
            ));
        }

        self.prepare_write_ir(data, len)?;
        let response = self.protocol.flush()?;
        tracing::trace!("Response: {:?}", response);

        // TODO: Why only store the first 8 bits?
        self.current_ir_reg = data[0] as u32;

        // Maybe we could return the previous state of the IR register here...
        Ok(())
    }

    fn prepare_write_ir(&mut self, data: &[u8], len: usize) -> Result<usize, DebugProbeError> {
        tracing::debug!("Write IR: {:?}, len={}", data, len);

        // Check the bit length, enough data has to be
        // available
        if data.len() * 8 < len {
            todo!("Proper error for incorrect length");
        }

        // At least one bit has to be sent
        if len < 1 {
            todo!("Proper error for incorrect length");
        }

        let tms_enter_ir_shift = [true, true, false, false];

        // The last bit will be transmitted when exiting the shift state,
        // so we need to stay in the shift state for one period less than
        // we have bits to transmit.
        let tms_data = iter::repeat(false).take(len - 1);

        let tms_enter_idle = [true, true, false];

        let mut tms = Vec::with_capacity(tms_enter_ir_shift.len() + len + tms_enter_idle.len());

        tms.extend_from_slice(&tms_enter_ir_shift);
        tms.extend(tms_data);
        tms.extend_from_slice(&tms_enter_idle);

        let tdi_enter_ir_shift = [false, false, false, false];

        // This is one less than the enter idle for tms, because
        // the last bit is transmitted when exiting the IR shift state
        let tdi_enter_idle = [false, false];

        let mut tdi = Vec::with_capacity(tdi_enter_ir_shift.len() + len + tdi_enter_idle.len());

        tdi.extend_from_slice(&tdi_enter_ir_shift);

        let num_bytes = len / 8;

        let num_bits = len - (num_bytes * 8);

        for bytes in &data[..num_bytes] {
            let mut byte = *bytes;

            for _ in 0..8 {
                tdi.push(byte & 1 == 1);

                byte >>= 1;
            }
        }

        if num_bits > 0 {
            let mut remaining_byte = data[num_bytes];

            for _ in 0..num_bits {
                tdi.push(remaining_byte & 1 == 1);
                remaining_byte >>= 1;
            }
        }

        tdi.extend_from_slice(&tdi_enter_idle);

        tracing::trace!("tms: {:?}", tms);
        tracing::trace!("tdi: {:?}", tdi);

        let len = tms.len();
        self.protocol.jtag_io_async(tms, tdi, true)?;

        Ok(len)
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        self.prepare_write_dr(data, register_bits)?;
        let mut response = self.protocol.flush()?;
        self.recieve_write_dr(response.iter(), register_bits)
    }

    fn recieve_write_dr(
        &mut self,
        mut response: BitIter,
        register_bits: usize,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let tms_enter_shift = [true, false, false]; // TODO taken from below, make a const in future

        let _remainder = response.split_off(tms_enter_shift.len());

        let mut remaining_bits = register_bits;

        let mut result = Vec::new();

        while remaining_bits >= 8 {
            let byte = bits_to_byte(response.split_off(8)) as u8;
            result.push(byte);
            remaining_bits -= 8;
        }

        // Handle leftover bytes
        if remaining_bits > 0 {
            result.push(bits_to_byte(response.split_off(remaining_bits)) as u8);
        }

        tracing::trace!("result: {:?}", result);

        Ok(result)
    }

    fn prepare_write_dr(
        &mut self,
        data: &[u8],
        register_bits: usize,
    ) -> Result<usize, DebugProbeError> {
        tracing::debug!("Write DR: {:?}, len={}", data, register_bits);

        let tms_enter_shift = [true, false, false];

        // Last bit of data is shifted out when we exi the SHIFT-DR State
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        let tms_enter_idle = [true, true, false];

        let mut tms = Vec::with_capacity(register_bits + 7);

        tms.extend_from_slice(&tms_enter_shift);
        tms.extend(tms_shift_out_value);
        tms.extend_from_slice(&tms_enter_idle);

        let tdi_enter_shift = [false, false, false];

        let tdi_enter_idle = [false, false];

        let mut tdi =
            Vec::with_capacity(tdi_enter_shift.len() + tdi_enter_idle.len() + register_bits);

        tdi.extend_from_slice(&tdi_enter_shift);

        let num_bytes = register_bits / 8;

        let num_bits = register_bits - (num_bytes * 8);

        for bytes in &data[..num_bytes] {
            let mut byte = *bytes;

            for _ in 0..8 {
                tdi.push(byte & 1 == 1);

                byte >>= 1;
            }
        }

        if num_bits > 0 {
            let mut remaining_byte = data[num_bytes];

            for _ in 0..num_bits {
                tdi.push(remaining_byte & 1 == 1);
                remaining_byte >>= 1;
            }
        }

        tdi.extend_from_slice(&tdi_enter_idle);

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
        let address_bits = address.to_le_bytes();

        // TODO: This is limited to 5 bit addresses for now
        if address > 0x1f {
            return Err(DebugProbeError::NotImplemented(
                "JTAG Register addresses are fixed to 5 bits",
            ));
        }

        let write_ir_bits = if self.current_ir_reg != address {
            // Write IR register
            let def = self.prepare_write_ir(&address_bits[..1], 5)?;
            self.current_ir_reg = data[0] as u32;
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
        let address_bits = address.to_le_bytes();

        // TODO: This is limited to 5 bit addresses for now
        if address > 0x1f {
            return Err(DebugProbeError::NotImplemented(
                "JTAG Register addresses are fixed to 5 bits",
            ));
        }

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&address_bits[..1], 5)?;
        }

        // read DR register
        self.read_dr(len as usize)
    }

    /// Write the data register
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let address_bits = address.to_le_bytes();

        // TODO: This is limited to 5 bit addresses for now
        if address > 0x1f {
            return Err(DebugProbeError::NotImplemented(
                "JTAG Register addresses are fixed to 5 bits",
            ));
        }

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&address_bits[..1], 5)?;
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
        let mut response = response.iter();

        let mut responses = Vec::with_capacity(bits.len());

        for (index, bit) in bits.into_iter().enumerate() {
            if let Some(ir_bits) = bit.write_ir_bits {
                _ = response.split_off(ir_bits);
            }
            let dr_resp = response.split_off(bit.write_dr_bits_total);
            let v = self
                .recieve_write_dr(dr_resp, bit.write_dr_bits)
                .map_err(|e| super::BatchExecutionError::new(e.into(), responses.clone()))?;

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

    fn attach(&mut self) -> Result<(), super::DebugProbeError> {
        tracing::debug!("Attaching to ESP USB JTAG");

        // TODO: Maybe can be left in protocol altogether.

        // try some JTAG stuff

        tracing::debug!("Resetting JTAG chain using trst");
        self.protocol.set_reset(true, true)?;
        self.protocol.set_reset(false, false)?;

        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = vec![true, true, true, true, true, false];
        let tdi = iter::repeat(false).take(6);

        let response: Vec<_> = self.protocol.jtag_io(tms, tdi, false)?.collect();

        tracing::debug!("Response to reset: {:?}", response);

        // try to read the idcode until we have some non-zero bytes
        let start = Instant::now();
        let idcode = loop {
            let idcode_bytes = self.read_dr(32)?;
            if idcode_bytes.iter().any(|&x| x != 0)
                || Instant::now().duration_since(start) > Duration::from_secs(1)
            {
                break u32::from_le_bytes((&idcode_bytes[..]).try_into().unwrap());
            }
        };

        tracing::info!("JTAG IDCODE: {:#010x}", idcode);

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
