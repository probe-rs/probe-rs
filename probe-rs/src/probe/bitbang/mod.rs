use crate::architecture::riscv::communication_interface::{
    RiscvCommunicationInterface, RiscvError,
};
use crate::probe::JTAGAccess;
use crate::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType,
    ProbeCreationError, WireProtocol,
};
use std::net::TcpStream;

mod bitbang_adapter;
mod bitbang_engine;

use bitbang_engine::{bool_slice_to_u8_slice, BitBangEngine};

#[derive(Debug)]
pub struct BitBangProbe {
    engine: BitBangEngine,
    speed_khz: u32,
    idle_cycles: u8,
    ir_len: u32,
}

impl DebugProbe for BitBangProbe {
    fn new_from_selector(
        _selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        /*
        // TODO match on the selector some how...
        let selector = selector.into();

        // Only open FTDI probes
        if selector.vendor_id != 0xFFFF {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }

        // let adapter = JtagAdapter::open(selector.vendor_id, selector.product_id)
        */

        let socket = TcpStream::connect("127.0.0.1:44853")
            .map_err(|_e| DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound))?;
        let engine =
            BitBangEngine::new(socket).map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let probe = BitBangProbe {
            engine,
            speed_khz: 0,
            idle_cycles: 0,
            ir_len: 32,
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn get_name(&self) -> &str {
        "BitBang"
    }

    // TODO some way of regulating speed/reporting it as not supported?
    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    // TODO some way of regulating speed/reporting it as not supported?
    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.speed_khz = speed_khz;
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("attaching...");

        // Do a quick tap reset
        // TODO proper error mapping?
        self.engine
            .reset(true, false)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        // We will use this to map the DebugProbeSelector, maybe?
        /*
        let idcode = self.engine.write_read_dr(&[false;32]).map_err(|_e| DebugProbeError::TargetNotFound)?;
        let idcode = bool_slice_to_u8_slice(&idcode, 32);
        tracing::debug!("found idcode {:x?}", idcode);
        */

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        // TODO proper error mapping
        self.engine.quit().unwrap();
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::trace!("BitBang target_reset");
        unimplemented!()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.engine.adapter.reset(true, true).unwrap();
        tracing::trace!("BitBang target_reset_assert");
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.engine.adapter.reset(false, false).unwrap();
        tracing::trace!("BitBang target_reset_deassert");
        Ok(())
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
}

impl JTAGAccess for BitBangProbe {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        tracing::trace!("read_register({:#x}, {})", address, len);
        // convert data u8 slice into bool slice
        let mut bool_data = vec![];
        for _ in 0..len {
            bool_data.push(false)
        }
        let data = self
            .engine
            .write_read_register(address, self.ir_len, &bool_data)
            .map_err(|e| {
                tracing::error!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;

        let read_data = bool_slice_to_u8_slice(&data, len as usize);

        self.engine
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        tracing::trace!("read_register result: {:#x?})", read_data);
        Ok(read_data)
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        tracing::trace!("set_idle_cycles({})", idle_cycles);
        self.idle_cycles = idle_cycles;
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        tracing::trace!("write_register({:#x}, {:#x?}, {})", address, data, len);

        // convert data u8 slice into bool slice
        let mut bool_data = vec![];
        for i in 0..len {
            bool_data.push((data[i as usize / 8] & (1 << (i % 8))) != 0)
        }

        let r = self
            .engine
            .write_read_register(address, self.ir_len, &bool_data)
            .map_err(|e| {
                tracing::error!("target_transfer error: {:?}", e);
                DebugProbeError::ProbeSpecific(Box::new(e))
            })?;

        let read_data = bool_slice_to_u8_slice(&r, len as usize);

        self.engine
            .idle(self.idle_cycles as usize)
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
        tracing::trace!("write_register result: {:#x?})", read_data);
        Ok(read_data)
    }

    fn get_idle_cycles(&self) -> u8 {
        self.idle_cycles
    }

    fn set_ir_len(&mut self, len: u32) {
        self.ir_len = len;
    }
}

#[tracing::instrument(skip_all)]
pub(crate) fn list_bitbang_devices() -> Vec<DebugProbeInfo> {
    let mut devs = vec![];
    devs.push(DebugProbeInfo {
        identifier: "BitBang Interface".to_owned(),
        vendor_id: 0,
        product_id: 0,
        serial_number: None,
        probe_type: DebugProbeType::BitBang,
        hid_interface: None,
    });

    devs
}
