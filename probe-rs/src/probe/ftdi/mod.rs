use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::{
    arm::communication_interface::UninitializedArmProbe,
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::probe::{JTAGAccess, ProbeCreationError};
use crate::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType, WireProtocol,
};
use bitvec::{order::Lsb0, slice::BitSlice, vec::BitVec};
use rusb::UsbContext;
use std::convert::TryInto;
use std::io::{self, Read, Write};
use std::time::Duration;

mod ftdi_impl;
use ftdi_impl as ftdi;

mod commands;

use self::commands::{JtagCommand, WriteRegisterCommand};

use super::{BatchExecutionError, CommandResult};

#[derive(Debug)]
struct JtagChainItem {
    idcode: u32,
    irlen: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ChainParams {
    irpre: usize,
    irpost: usize,
    drpre: usize,
    drpost: usize,
    irlen: usize,
}

#[derive(Debug)]
pub struct JtagAdapter {
    device: ftdi::Device,
    chain_params: Option<ChainParams>,
}

impl JtagAdapter {
    pub fn open(vid: u16, pid: u16) -> Result<Self, ftdi::Error> {
        let mut builder = ftdi::Builder::new();
        builder.set_interface(ftdi::Interface::A)?;
        let device = builder.usb_open(vid, pid)?;

        Ok(Self {
            device,
            chain_params: None,
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

    fn read_response(&mut self, size: usize) -> io::Result<Vec<u8>> {
        let timeout = Duration::from_millis(10);
        let mut result = Vec::new();

        let t0 = std::time::Instant::now();
        while result.len() < size {
            if t0.elapsed() > timeout {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }

            self.device.read_to_end(&mut result)?;
        }

        if result.len() > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Read more data than expected",
            ));
        }

        Ok(result)
    }

    fn shift_tms(&mut self, mut data: &[u8], mut bits: usize) -> io::Result<()> {
        assert!(bits > 0);
        assert!((bits + 7) / 8 <= data.len());

        let mut command = vec![];

        while bits > 0 {
            if bits >= 8 {
                command.extend_from_slice(&[0x4b, 0x07, data[0]]);
                data = &data[1..];
                bits -= 8;
            } else {
                command.extend_from_slice(&[0x4b, (bits - 1) as u8, data[0]]);
                bits = 0;
            }
        }
        self.device.write_all(&command)
    }

    fn shift_tdi(&mut self, mut data: &[u8], mut bits: usize) -> io::Result<()> {
        assert!(bits > 0);
        assert!((bits + 7) / 8 <= data.len());

        let mut command = vec![];

        let full_bytes = (bits - 1) / 8;
        if full_bytes > 0 {
            assert!(full_bytes <= 65536);

            command.extend_from_slice(&[0x19]);
            let n: u16 = (full_bytes - 1) as u16;
            command.extend_from_slice(&n.to_le_bytes());
            command.extend_from_slice(&data[..full_bytes]);

            bits -= full_bytes * 8;
            data = &data[full_bytes..];
        }
        assert!(bits <= 8);

        if bits > 0 {
            let byte = data[0];
            if bits > 1 {
                let n = (bits - 2) as u8;
                command.extend_from_slice(&[0x1b, n, byte]);
            }

            let last_bit = (byte >> (bits - 1)) & 0x01;
            let tms_byte = 0x01 | (last_bit << 7);
            command.extend_from_slice(&[0x4b, 0x00, tms_byte]);
        }

        self.device.write_all(&command)
    }

    fn tranfer_tdi(&mut self, mut data: &[u8], mut bits: usize) -> io::Result<Vec<u8>> {
        assert!(bits > 0);
        assert!((bits + 7) / 8 <= data.len());

        let mut command = vec![];

        let full_bytes = (bits - 1) / 8;
        if full_bytes > 0 {
            assert!(full_bytes <= 65536);

            command.extend_from_slice(&[0x39]);
            let n: u16 = (full_bytes - 1) as u16;
            command.extend_from_slice(&n.to_le_bytes());
            command.extend_from_slice(&data[..full_bytes]);

            bits -= full_bytes * 8;
            data = &data[full_bytes..];
        }
        assert!(0 < bits && bits <= 8);

        let byte = data[0];
        if bits > 1 {
            let n = (bits - 2) as u8;
            command.extend_from_slice(&[0x3b, n, byte]);
        }

        let last_bit = (byte >> (bits - 1)) & 0x01;
        let tms_byte = 0x01 | (last_bit << 7);
        command.extend_from_slice(&[0x6b, 0x00, tms_byte]);

        self.device.write_all(&command)?;

        let mut expect_bytes = full_bytes + 1;
        if bits > 1 {
            expect_bytes += 1;
        }

        let mut reply = self.read_response(expect_bytes)?;

        let mut last_byte = reply[reply.len() - 1] >> 7;
        if bits > 1 {
            let byte = reply[reply.len() - 2] >> (8 - (bits - 1));
            last_byte = byte | (last_byte << (bits - 1));
        }
        reply[full_bytes] = last_byte;
        reply.truncate(full_bytes + 1);

        Ok(reply)
    }

    /// Reset and go to RUN-TEST/IDLE
    pub fn reset(&mut self) -> io::Result<()> {
        self.shift_tms(&[0xff, 0xff, 0xff, 0xff, 0x7f], 40)
    }

    /// Execute RUN-TEST/IDLE for a number of cycles
    pub fn idle(&mut self, cycles: usize) -> io::Result<()> {
        if cycles == 0 {
            return Ok(());
        }
        let mut buf = vec![];
        buf.resize((cycles + 7) / 8, 0);
        self.shift_tms(&buf, cycles)
    }

    /// Shift to IR and return to IDLE
    pub fn shift_ir(&mut self, data: &[u8], bits: usize) -> io::Result<()> {
        self.shift_tms(&[0b0011], 4)?;
        self.shift_tdi(data, bits)?;
        self.shift_tms(&[0b01], 2)?;
        Ok(())
    }

    /// Shift to IR and return to IDLE
    pub fn transfer_ir(&mut self, data: &[u8], bits: usize) -> io::Result<Vec<u8>> {
        self.shift_tms(&[0b0011], 4)?;
        let r = self.tranfer_tdi(data, bits)?;
        self.shift_tms(&[0b01], 2)?;
        Ok(r)
    }

    /// Shift to DR and return to IDLE
    pub fn transfer_dr(&mut self, data: &[u8], bits: usize) -> io::Result<Vec<u8>> {
        self.shift_tms(&[0b001], 3)?;
        let r = self.tranfer_tdi(data, bits)?;
        self.shift_tms(&[0b01], 2)?;
        Ok(r)
    }

    fn scan(&mut self) -> io::Result<Vec<JtagChainItem>> {
        let max_device_count = 8;

        self.reset()?;

        let cmd = vec![0xff; max_device_count * 4];
        let r = self.transfer_dr(&cmd, cmd.len() * 8)?;
        let mut targets = vec![];
        for i in 0..max_device_count {
            let idcode = u32::from_le_bytes(r[i * 4..(i + 1) * 4].try_into().unwrap());
            if idcode != 0xffffffff {
                tracing::debug!("tap found: {:08x}", idcode);
                let target = JtagChainItem { idcode, irlen: 0 };
                targets.push(target);
            } else {
                break;
            }
        }

        self.reset()?;

        // Autodetect the targets' IR lengths.
        //
        // For many targets, reading the IR right after a reset yields 0b00..001. This allows
        // autodetecting the IR lengths even when we have multiple targets. For example,
        // if we read `0b1111111111110001000001` (LSB first) we know the first target in the
        // chain has an irlen of 6 and the next one has an irlen of 4.
        //
        // However, not all targets satisfy this. For example, the esp32c3 shifts out a fixed value
        // of `0b00101`. This makes the above algorithm to incorrectly detect the IR len as 2.
        //
        // Fortunately, we can use a different autodetection algorithm when we only have one target
        // in the chain, that doesn't rely on the target to shift out a particular value. The key is
        // the fact that whatever we shift in gets shifted back out, but delayed by the number of bits
        // in the IR shfit register. So, we shift in lots of `1` bits to fill the shift register with `1`s.
        // Then we shift in lots of `0` bytes. The output will be something like `0b00000111`, and the
        // number of ones is the IR length.
        if targets.len() == 1 {
            let r = self.transfer_ir(&[0xFF, 0x00], 16)?;

            let irlen = r[1].count_ones() as usize;
            targets[0].irlen = irlen;
            tracing::debug!("tap irlen: {}", irlen);
        } else {
            let cmd = vec![0xff; max_device_count];
            let mut r = self.transfer_ir(&cmd, cmd.len() * 8)?;

            let mut ir = 0;
            let mut irbits = 0;
            for (i, target) in targets.iter_mut().enumerate() {
                if (!r.is_empty()) && irbits < 8 {
                    let byte = r[0];
                    r.remove(0);
                    ir |= (byte as u32) << irbits;
                    irbits += 8;
                }
                if ir & 0b11 == 0b01 {
                    ir &= !1;
                    let irlen = ir.trailing_zeros();
                    ir >>= irlen;
                    irbits -= irlen;
                    tracing::debug!("tap {} irlen: {}", i, irlen);
                    target.irlen = irlen as usize;
                } else {
                    tracing::debug!("invalid irlen for tap {}", i);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid IR sequence during the chain scan",
                    ));
                }
            }
        }

        Ok(targets)
    }

    pub fn select_target(&mut self, idcode: u32) -> io::Result<()> {
        let taps = self.scan()?;

        let mut found = false;
        let mut params = ChainParams {
            irpre: 0,
            irpost: 0,
            drpre: 0,
            drpost: 0,
            irlen: 0,
        };
        for tap in taps {
            if tap.idcode == idcode {
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

        if found {
            tracing::debug!("Target chain params: {:?}", params);
            self.chain_params = Some(params);
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "target not found"))
        }
    }

    fn get_chain_params(&self) -> io::Result<ChainParams> {
        match &self.chain_params {
            Some(params) => Ok(*params),
            None => Err(io::Error::new(
                io::ErrorKind::Other,
                "target is not selected",
            )),
        }
    }

    fn target_transfer(
        &mut self,
        address: u32,
        data: Option<&[u8]>,
        len_bits: usize,
    ) -> io::Result<Vec<u8>> {
        let params = self.get_chain_params()?;
        let max_address = (1 << params.irlen) - 1;
        if address > max_address {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid register address",
            ));
        }

        // Write IR register
        let irbits = params.irpre + params.irlen + params.irpost;
        assert!(irbits <= 32);
        let mut ir: u32 = (1 << params.irpre) - 1;
        ir |= address << params.irpre;
        ir |= ((1 << params.irpost) - 1) << (params.irpre + params.irlen);
        self.shift_ir(&ir.to_le_bytes(), irbits)?;

        let drbits = params.drpre + len_bits + params.drpost;
        let request = if let Some(data_slice) = data {
            let data = BitSlice::<u8, Lsb0>::from_slice(data_slice);
            let mut data = BitVec::<u8, Lsb0>::from_bitslice(data);
            data.truncate(len_bits);

            let mut buf = BitVec::<u8, Lsb0>::new();
            buf.resize(params.drpre, false);
            buf.append(&mut data);
            buf.resize(buf.len() + params.drpost, false);

            buf.into_vec()
        } else {
            vec![0; (drbits + 7) / 8]
        };
        let reply = self.transfer_dr(&request, drbits)?;

        // Process the reply
        let mut reply = BitVec::<u8, Lsb0>::from_vec(reply);
        if params.drpre > 0 {
            reply = reply.split_off(params.drpre);
        }
        reply.truncate(len_bits);
        let reply = reply.into_vec();

        Ok(reply)
    }
}

#[derive(Debug)]
pub struct FtdiProbe {
    adapter: JtagAdapter,
    speed_khz: u32,
    idle_cycles: u8,
}

impl DebugProbe for FtdiProbe {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        let selector = selector.into();

        // Only open FTDI probes
        if selector.vendor_id != 0x0403 {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }

        let adapter = JtagAdapter::open(selector.vendor_id, selector.product_id)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let probe = FtdiProbe {
            adapter,
            speed_khz: 0,
            idle_cycles: 0,
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

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

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("attaching...");

        self.adapter
            .attach()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let taps = self
            .adapter
            .scan()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        if taps.is_empty() {
            tracing::warn!("no JTAG taps detected");
            return Err(DebugProbeError::TargetNotFound);
        }
        if taps.len() == 1 {
            self.adapter
                .select_target(taps[0].idcode)
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        } else {
            let known_idcodes = [
                0x1000563d, // GD32VF103
            ];
            let idcode = taps
                .iter()
                .map(|tap| tap.idcode)
                .find(|idcode| known_idcodes.iter().any(|v| v == idcode));
            if let Some(idcode) = idcode {
                self.adapter
                    .select_target(idcode)
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
}

impl JTAGAccess for FtdiProbe {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("read_register({:#x}, {})", address, len);
        let r = self
            .adapter
            .target_transfer(address, None, len as usize)
            .map_err(|e| {
                tracing::debug!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;
        self.adapter
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        tracing::debug!("read_register result: {:?})", r);
        Ok(r)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        tracing::debug!("set_idle_cycles({})", idle_cycles);
        self.idle_cycles = idle_cycles;
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("write_register({:#x}, {:?}, {})", address, data, len);
        let r = self
            .adapter
            .target_transfer(address, Some(data), len as usize)
            .map_err(|e| {
                tracing::debug!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;
        self.adapter
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        tracing::debug!("write_register result: {:?})", r);
        Ok(r)
    }

    fn get_idle_cycles(&self) -> u8 {
        self.idle_cycles
    }

    fn write_register_batch(
        &mut self,
        writes: &[super::JtagWriteCommand],
    ) -> Result<Vec<CommandResult>, BatchExecutionError> {
        // this value was determined by experimenting and doesn't match e.g
        // the libftdi read/write chunk size - it is hopefully useful for every setup
        // max value seems to be different for different adapters, e.g. for the Sipeed JTAG adapter
        // 40 works but for the Pine64 adapter it doesn't
        const CHUNK_SIZE: usize = 30;

        let mut index_offset = 0;
        let mut results = Vec::<CommandResult>::new();

        let chain_params = self.adapter.get_chain_params().map_err(|e| {
            BatchExecutionError::new(
                crate::Error::Probe(DebugProbeError::ProbeSpecific(Box::new(e))),
                results.clone(),
            )
        })?;

        let commands: Result<Vec<WriteRegisterCommand>, _> = writes
            .iter()
            .map(|w| {
                WriteRegisterCommand::new(
                    w.address,
                    w.data.clone(),
                    w.len as usize,
                    self.idle_cycles as usize,
                    chain_params,
                )
            })
            .collect();

        let mut commands = commands.map_err(|e| {
            BatchExecutionError::new(
                crate::Error::Probe(DebugProbeError::ProbeSpecific(Box::new(e))),
                results.clone(),
            )
        })?;

        for cmd_chunk in commands.chunks_mut(CHUNK_SIZE) {
            let mut out_buffer = Vec::<u8>::new();
            let mut size = 0;
            for cmd in cmd_chunk.iter_mut() {
                cmd.add_bytes(&mut out_buffer);
                size += cmd.bytes_to_read();
            }

            // Send Immediate: This will make the FTDI chip flush its buffer back to the PC.
            // See https://www.ftdichip.com/Support/Documents/AppNotes/AN_108_Command_Processor_for_MPSSE_and_MCU_Host_Bus_Emulation_Modes.pdf
            // section 5.1
            out_buffer.push(0x87);

            let write_res = self.adapter.device.write_all(&out_buffer);
            match write_res {
                Ok(_) => (),
                Err(e) => {
                    return Err(BatchExecutionError::new(
                        crate::Error::Probe(DebugProbeError::ProbeSpecific(Box::new(e))),
                        results.clone(),
                    ));
                }
            }

            let timeout = Duration::from_millis(10);
            let mut result = Vec::new();

            let t0 = std::time::Instant::now();
            while result.len() < size {
                if t0.elapsed() > timeout {
                    return Err(BatchExecutionError::new(
                        crate::Error::Probe(DebugProbeError::Timeout),
                        results.clone(),
                    ));
                }

                let read_res = self.adapter.device.read_to_end(&mut result);
                match read_res {
                    Ok(_) => (),
                    Err(e) => {
                        return Err(BatchExecutionError::new(
                            crate::Error::Probe(DebugProbeError::ProbeSpecific(Box::new(e))),
                            results.clone(),
                        ));
                    }
                }
            }

            if result.len() > size {
                return Err(BatchExecutionError::new(
                    crate::Error::Probe(DebugProbeError::ProbeSpecific(Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Read more data than expected",
                    )))),
                    results.clone(),
                ));
            }

            let mut pos = 0;
            for (index, cmd) in cmd_chunk.iter().enumerate() {
                let index = index + index_offset;
                let len = cmd.bytes_to_read();
                let mut data = Vec::<u8>::new();
                data.extend_from_slice(&result[pos..(pos + len)]);

                let result = cmd.process_output(&data);

                match result {
                    Ok(data) => {
                        let transformer = writes[index].transform;

                        let data = match data {
                            CommandResult::VecU8(data) => data,
                            _ => panic!("Internal error occurred. Cannot have a transformer function for outputs other than Vec<u8>"),
                        };
                        results.push(
                            transformer(data)
                                .map_err(|e| BatchExecutionError::new(e, results.clone()))?,
                        );
                    }
                    Err(e) => {
                        return Err(BatchExecutionError::new(
                            crate::Error::Probe(e),
                            results.clone(),
                        ))
                    }
                }

                pos += len;
            }

            index_offset += cmd_chunk.len();
        }

        Ok(results)
    }

    fn set_ir_len(&mut self, _len: u32) {
        // The FTDI implementation automatically sets this, so need need to act on this data
    }
}

/// (VendorId, ProductId)
static FTDI_COMPAT_DEVICE_IDS: &[(u16, u16)] = &[(0x0403, 0x6010), (0x0403, 0x6014)];

fn get_device_info(device: &rusb::Device<rusb::Context>) -> Option<DebugProbeInfo> {
    let d_desc = device.device_descriptor().ok()?;

    if !FTDI_COMPAT_DEVICE_IDS
        .iter()
        .any(|(vid, pid)| d_desc.vendor_id() == *vid && d_desc.product_id() == *pid)
    {
        return None;
    }

    let handle = match device.open() {
        Err(rusb::Error::Access) => {
            tracing::warn!("Access denied: probe device {:#?}", device);
            return None;
        }
        Err(e) => {
            tracing::warn!("Can't open probe device {:#?} -- Error: {:#?}", device, e);
            return None;
        }
        Ok(v) => v,
    };

    let prod_str = handle.read_product_string_ascii(&d_desc).ok()?;
    let sn_str = handle.read_serial_number_string_ascii(&d_desc).ok();

    Some(DebugProbeInfo {
        identifier: prod_str,
        vendor_id: d_desc.vendor_id(),
        product_id: d_desc.product_id(),
        serial_number: sn_str,
        probe_type: DebugProbeType::Ftdi,
        hid_interface: None,
    })
}

#[tracing::instrument(skip_all)]
pub(crate) fn list_ftdi_devices() -> Vec<DebugProbeInfo> {
    match rusb::Context::new().and_then(|ctx| ctx.devices()) {
        Ok(devices) => devices
            .iter()
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(_) => vec![],
    }
}
