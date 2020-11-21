use crate::architecture::riscv::communication_interface::RiscvCommunicationInterface;
use crate::probe::{JTAGAccess, ProbeCreationError};
use crate::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType, WireProtocol,
};
use bitvec::{order::Lsb0, slice::BitSlice, vec::BitVec};
use rusb::UsbContext;
use std::convert::TryInto;
use std::io::{self, Read, Write};
use std::sync::Mutex;
use std::time::Duration;

mod ftdi_impl;
use ftdi_impl as ftdi;

#[derive(Debug)]
struct JtagChainItem {
    idcode: u32,
    irlen: usize,
}

#[derive(Clone, Debug)]
struct ChainParams {
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
                log::debug!("tap found: {:08x}", idcode);
                let target = JtagChainItem { idcode, irlen: 0 };
                targets.push(target);
            } else {
                break;
            }
        }

        self.reset()?;
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
                log::debug!("tap {} irlen: {}", i, irlen);
                target.irlen = irlen as usize;
            } else {
                log::debug!("invalid irlen for tap {}", i);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid IR sequence during the chain scan",
                ));
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
            log::debug!("Target chain params: {:?}", params);
            self.chain_params = Some(params);
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "target not found"))
        }
    }

    fn get_chain_params(&self) -> io::Result<ChainParams> {
        match &self.chain_params {
            Some(params) => Ok(params.clone()),
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
            let data = BitSlice::<Lsb0, u8>::from_slice(data_slice).ok_or(io::Error::new(
                io::ErrorKind::InvalidData,
                "could not create bitslice",
            ))?;
            let mut data = BitVec::<Lsb0, u8>::from_bitslice(&data);
            data.truncate(len_bits);

            let mut buf = BitVec::<Lsb0, u8>::new();
            buf.resize(params.drpre, false);
            buf.append(&mut data);
            buf.resize(buf.len() + params.drpost, false);

            buf.into_vec()
        } else {
            vec![0; (drbits + 7) / 8]
        };
        let reply = self.transfer_dr(&request, drbits)?;

        // Process the reply
        let mut reply = BitVec::<Lsb0, u8>::from_vec(reply);
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
    adapter: Mutex<JtagAdapter>,
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
            adapter: Mutex::new(adapter),
            speed_khz: 0,
            idle_cycles: 0,
        };
        log::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn get_name(&self) -> &str {
        "FTDI"
    }

    fn speed(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.speed_khz = speed_khz;
        // TODO
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("attaching...");
        let adapter = self.adapter.get_mut().unwrap();

        adapter
            .attach()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let taps = adapter
            .scan()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        if taps.is_empty() {
            log::warn!("no JTAG taps detected");
            return Err(DebugProbeError::TargetNotFound);
        }
        if taps.len() == 1 {
            adapter
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
                adapter
                    .select_target(idcode)
                    .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
            } else {
                return Err(DebugProbeError::TargetNotFound);
            }
        }
        Ok(())
    }

    fn detach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        log::error!("FTDI target_reset");
        unimplemented!()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        log::error!("FTDI target_reset_assert");
        unimplemented!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        log::error!("FTDI target_reset_deassert");
        unimplemented!()
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if protocol != WireProtocol::Jtag {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn get_riscv_interface(
        self: Box<Self>,
    ) -> Result<Option<RiscvCommunicationInterface>, DebugProbeError> {
        Ok(Some(RiscvCommunicationInterface::new(self)?))
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }
}

impl JTAGAccess for FtdiProbe {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        log::debug!("read_register({:#x}, {})", address, len);
        let adapter = self.adapter.get_mut().unwrap();
        let r = adapter
            .target_transfer(address, None, len as usize)
            .map_err(|e| {
                log::debug!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;
        adapter
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        log::debug!("read_register result: {:?})", r);
        Ok(r)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        log::debug!("set_idle_cycles({})", idle_cycles);
        self.idle_cycles = idle_cycles;
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        log::debug!("write_register({:#x}, {:?}, {})", address, data, len);
        let adapter = self.adapter.get_mut().unwrap();
        let r = adapter
            .target_transfer(address, Some(data), len as usize)
            .map_err(|e| {
                log::debug!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;
        adapter
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        log::debug!("write_register result: {:?})", r);
        Ok(r)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

impl AsRef<dyn DebugProbe> for FtdiProbe {
    fn as_ref(&self) -> &(dyn DebugProbe + 'static) {
        self
    }
}

impl AsMut<dyn DebugProbe> for FtdiProbe {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'static) {
        self
    }
}

fn get_device_info(device: &rusb::Device<rusb::Context>) -> Option<DebugProbeInfo> {
    let d_desc = device.device_descriptor().ok()?;
    if d_desc.vendor_id() != 0x0403 || d_desc.product_id() != 0x6010 {
        return None;
    }

    let handle = device.open().ok()?;

    let prod_str = handle.read_product_string_ascii(&d_desc).ok()?;
    let sn_str = handle.read_serial_number_string_ascii(&d_desc).ok();

    Some(DebugProbeInfo {
        identifier: prod_str,
        vendor_id: d_desc.vendor_id(),
        product_id: d_desc.product_id(),
        serial_number: sn_str,
        probe_type: DebugProbeType::FTDI,
    })
}

pub(crate) fn list_ftdi_devices() -> Vec<DebugProbeInfo> {
    match rusb::Context::new().and_then(|ctx| ctx.devices()) {
        Ok(devices) => devices
            .iter()
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(_) => vec![],
    }
}
