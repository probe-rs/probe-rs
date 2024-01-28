mod tools;

use crate::architecture::arm::communication_interface::{DapProbe, UninitializedArmProbe};
use crate::architecture::arm::{
    ArmCommunicationInterface, ArmError, DpAddress, PortType, RawDapAccess, SwoAccess, SwoConfig,
    SwoMode,
};
use crate::architecture::riscv::communication_interface::{
    RiscvCommunicationInterface, RiscvError,
};
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::probe::cmsisdap::commands::CmsisDapDevice;
use crate::probe::cmsisdap::CmsisDap;
use crate::probe::usb_util::InterfaceExt;
use crate::{
    CoreStatus, DebugProbe, DebugProbeError, DebugProbeSelector, Error, ProbeDriver, WireProtocol,
};
use nusb::transfer::{Control, ControlType, Recipient};
use probe_rs_target::ScanChainElement;
use std::time::Duration;

const RQ_SET_TWIDTH: u8 = 0x01;
const RQ_SET_SPEED: u8 = 0x02;

pub struct OrbTraceSource;

impl std::fmt::Debug for OrbTraceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrbTrace").finish()
    }
}

impl ProbeDriver for OrbTraceSource {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        Ok(Box::new(OrbTrace::new_from_device(
            tools::open_device_from_selector(selector)?,
        )?))
    }

    fn list_probes(&self) -> Vec<crate::DebugProbeInfo> {
        tools::list_orbtrace_devices()
    }
}

struct OrbTraceDevice {
    cmsis_dap: CmsisDapDevice,
    trace: TraceInterface,
}

struct OrbTrace {
    dap: Box<CmsisDap>,
    trace: TraceInterface,
}

impl OrbTrace {
    fn new_from_device(device: OrbTraceDevice) -> Result<Self, DebugProbeError> {
        let dap = CmsisDap::new_from_device(device.cmsis_dap)?;
        Ok(Self {
            dap: Box::new(dap),
            trace: device.trace,
        })
    }
}

impl std::fmt::Debug for OrbTrace {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("OrbTrace")
            .field("dap", &self.dap)
            .finish()
    }
}

impl DapProbe for OrbTrace {}

impl RawDapAccess for OrbTrace {
    fn select_dp(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        self.dap.select_dp(dp)
    }

    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, ArmError> {
        self.dap.raw_read_register(port, addr)
    }

    fn raw_read_block(
        &mut self,
        port: PortType,
        addr: u8,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        self.dap.raw_read_block(port, addr, values)
    }

    fn raw_write_register(&mut self, port: PortType, addr: u8, value: u32) -> Result<(), ArmError> {
        self.dap.raw_write_register(port, addr, value)
    }

    fn raw_write_block(
        &mut self,
        port: PortType,
        addr: u8,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.dap.raw_write_block(port, addr, values)
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        self.dap.raw_flush()
    }

    fn configure_jtag(&mut self) -> Result<(), DebugProbeError> {
        self.dap.configure_jtag()
    }

    fn jtag_sequence(&mut self, cycles: u8, tms: bool, tdi: u64) -> Result<(), DebugProbeError> {
        self.dap.jtag_sequence(cycles, tms, tdi)
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.dap.swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.dap.swj_pins(pin_out, pin_select, pin_wait)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn core_status_notification(&mut self, state: CoreStatus) -> Result<(), DebugProbeError> {
        self.dap.core_status_notification(state)
    }
}

impl DebugProbe for OrbTrace {
    fn get_name(&self) -> &str {
        self.dap.get_name()
    }

    fn speed_khz(&self) -> u32 {
        self.dap.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.dap.set_speed(speed_khz)
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        self.dap.set_scan_chain(scan_chain)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.dap.attach()
    }

    fn detach(&mut self) -> Result<(), Error> {
        self.dap.detach()
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.dap.target_reset()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.dap.target_reset_assert()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.dap.target_reset_deassert()
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        self.dap.select_protocol(protocol)
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        self.dap.active_protocol()
    }

    fn has_arm_interface(&self) -> bool {
        self.dap.has_arm_interface()
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        Ok(Box::new(ArmCommunicationInterface::new(self, false)))
    }

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        self.dap.try_get_riscv_interface()
    }

    fn has_riscv_interface(&self) -> bool {
        self.dap.has_riscv_interface()
    }

    fn try_get_xtensa_interface(
        self: Box<Self>,
    ) -> Result<XtensaCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        self.dap.try_get_xtensa_interface()
    }

    fn has_xtensa_interface(&self) -> bool {
        self.dap.has_xtensa_interface()
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        Some(&self.trace as _)
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        Some(&mut self.trace as _)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        self.dap.try_as_dap_probe()
    }

    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        self.dap.get_target_voltage()
    }
}

struct TraceInterface {
    handle: nusb::Interface,
    interface_number: u8,
    endpoint: u8,
    max_packet_size: usize,
    tracing_active: bool,
}

impl TraceInterface {
    fn control_out(
        &self,
        request: u8,
        value: u16,
        index_hi: u8,
        data: &[u8],
    ) -> Result<(), DebugProbeError> {
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value,
            index: u16::from(self.interface_number) | (u16::from(index_hi) << 8),
        };
        self.handle
            .control_out_blocking(control, data, Duration::from_millis(100))
            .map_err(|e| DebugProbeError::Usb(e.into()))?;
        Ok(())
    }

    fn set_trace_input_format(&self, format: TraceInputFormat) -> Result<(), DebugProbeError> {
        self.control_out(RQ_SET_TWIDTH, format as u16, 0, &[])
    }

    fn set_swo_speed(&self, baudrate: u32) -> Result<(), DebugProbeError> {
        let baudrate = baudrate.to_le_bytes();
        self.control_out(RQ_SET_SPEED, 0, 0, &baudrate)
    }
}

impl SwoAccess for TraceInterface {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        let format = match config.mode() {
            SwoMode::Uart => TraceInputFormat::SwoUart,
            SwoMode::Manchester => TraceInputFormat::SwoManchester,
        };
        self.set_trace_input_format(format)
            .map_err(ArmError::Probe)?;

        self.set_swo_speed(config.baud()).map_err(ArmError::Probe)?;

        self.tracing_active = true;

        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        self.set_trace_input_format(TraceInputFormat::Disabled)
            .map_err(ArmError::Probe)?;

        self.tracing_active = false;

        Ok(())
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        if self.tracing_active {
            let mut buf = vec![0u8; self.max_packet_size];
            match self.handle.read_bulk(self.endpoint, &mut buf, timeout) {
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    buf.truncate(0);
                    Ok(buf)
                }
                Err(e) => Err(ArmError::Probe(DebugProbeError::Usb(e))),
            }
        } else {
            Ok(Vec::new())
        }
    }

    fn swo_poll_interval_hint(&mut self, _config: &SwoConfig) -> Option<Duration> {
        Some(Duration::from_secs(0))
    }
}

impl Drop for TraceInterface {
    fn drop(&mut self) {
        if self.tracing_active {
            let _ = self.set_trace_input_format(TraceInputFormat::Disabled);
        }
    }
}

#[repr(u8)]
#[allow(dead_code)]
enum TraceInputFormat {
    /// Trace input disabled.
    Disabled = 0x00,
    /// 1-bit synchronous trace input.
    Trace1 = 0x01,
    /// 2-bit synchronous trace input.
    Trace2 = 0x02,
    /// 4-bit synchronous trace input.
    Trace4 = 0x03,
    /// Manchester encoded asynchronous input (ITM).
    SwoManchester = 0x10,
    /// Manchester encoded asynchronous input (TPIU).
    ///
    /// Currently there is no difference between ITM and TPIU handling.
    SwoManchesterTpiu = 0x11,
    /// NRZ encoded asynchronous input (ITM).
    SwoUart = 0x12,
    /// NRZ encoded asynchronous input (TPIU).
    ///
    /// Currently there is no difference between ITM and TPIU handling.
    SwoUartTpiu = 0x13,
}
