//! Support for J-Link Debug probes

use jaylink::{Capability, Interface, JayLink, SpeedConfig, SwoMode};

use std::convert::{TryFrom, TryInto};
use std::iter;
use std::time::{Duration, Instant};

use crate::architecture::arm::{ArmError, RawDapAccess};
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::probe::common::bits_to_byte;
use crate::{
    architecture::{
        arm::{
            communication_interface::DapProbe, communication_interface::UninitializedArmProbe,
            swo::SwoConfig, ArmCommunicationInterface, SwoAccess,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
    },
    probe::{
        arm_jtag::{ProbeStatistics, RawProtocolIo, SwdSettings},
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeType, JTAGAccess, WireProtocol,
    },
    DebugProbeSelector,
};

const SWO_BUFFER_SIZE: u16 = 128;

#[derive(Debug)]
pub(crate) struct JLink {
    handle: JayLink,
    swo_config: Option<SwoConfig>,

    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    jtag_idle_cycles: u8,

    // JTAG IR register length
    ir_len: usize,

    /// Currently selected protocol
    protocol: Option<WireProtocol>,

    /// Protocols supported by the connected J-Link probe.
    supported_protocols: Vec<WireProtocol>,

    current_ir_reg: u32,

    speed_khz: u32,

    probe_statistics: ProbeStatistics,
    swd_settings: SwdSettings,
}

impl JLink {
    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles
    }

    fn select_interface(
        &mut self,
        protocol: Option<WireProtocol>,
    ) -> Result<WireProtocol, DebugProbeError> {
        let capabilities = self.handle.capabilities();

        if capabilities.contains(Capability::SelectIf) {
            if let Some(protocol) = protocol {
                let jlink_interface = match protocol {
                    WireProtocol::Swd => jaylink::Interface::Swd,
                    WireProtocol::Jtag => jaylink::Interface::Jtag,
                };

                if self.handle.available_interfaces().contains(jlink_interface) {
                    // We can select the desired interface
                    self.handle.select_interface(jlink_interface)?;
                    Ok(protocol)
                } else {
                    Err(DebugProbeError::UnsupportedProtocol(protocol))
                }
            } else {
                // No special protocol request
                let current_protocol = self.handle.current_interface();

                match current_protocol {
                    jaylink::Interface::Swd => Ok(WireProtocol::Swd),
                    jaylink::Interface::Jtag => Ok(WireProtocol::Jtag),
                    x => unimplemented!("J-Link: Protocol {} is not yet supported.", x),
                }
            }
        } else {
            // Assume JTAG protocol if the probe does not support switching interfaces
            match protocol {
                Some(WireProtocol::Jtag) => Ok(WireProtocol::Jtag),
                Some(p) => Err(DebugProbeError::UnsupportedProtocol(p)),
                None => Ok(WireProtocol::Jtag),
            }
        }
    }

    fn read_dr(&mut self, register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        tracing::debug!("Read {} bits from DR", register_bits);

        let tms_enter_shift = [true, false, false];

        // Last bit of data is shifted out when we exi the SHIFT-DR State
        let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

        let tms_enter_idle = [true, true, false];

        let mut tms = Vec::with_capacity(register_bits + 7);

        tms.extend_from_slice(&tms_enter_shift);
        tms.extend(tms_shift_out_value);
        tms.extend_from_slice(&tms_enter_idle);

        let tdi = iter::repeat(false).take(tms.len() + self.idle_cycles() as usize);

        // We have to stay in the idle cycle a bit
        tms.extend(iter::repeat(false).take(self.idle_cycles() as usize));

        let mut response = self.handle.jtag_io(tms, tdi)?;

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
    /// IR register might have an odd length, so the dta
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(&mut self, data: &[u8], len: usize) -> Result<(), DebugProbeError> {
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
        // so we need to stay in the shift stay for one period less than
        // we have bits to transmit
        let tms_data = iter::repeat(false).take(len - 1);

        let tms_enter_idle = [true, true, false];

        let mut tms = Vec::with_capacity(tms_enter_ir_shift.len() + len + tms_enter_ir_shift.len());

        tms.extend_from_slice(&tms_enter_ir_shift);
        tms.extend(tms_data);
        tms.extend_from_slice(&tms_enter_idle);

        let tdi_enter_ir_shift = [false, false, false, false];

        // This is one less than the enter idle for tms, because
        // the last bit is transmitted when exiting the IR shift state
        let tdi_enter_idle = [false, false];

        let mut tdi = Vec::with_capacity(tdi_enter_ir_shift.len() + tdi_enter_idle.len() + len);

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

        let response = self.handle.jtag_io(tms, tdi)?;

        tracing::trace!("Response: {:?}", response);

        if len >= 8 {
            return Err(DebugProbeError::NotImplemented(
                "Not yet implemented for IR registers larger than 8 bit",
            ));
        }

        self.current_ir_reg = data[0] as u32;

        // Maybe we could return the previous state of the IR register here...

        Ok(())
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
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

        // TODO: TDI data
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

        let mut response = self.handle.jtag_io(tms, tdi)?;

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

        tracing::trace!("result: {:?}", result);

        Ok(result)
    }
}

impl DebugProbe for JLink {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError> {
        let selector = selector.into();
        let mut jlinks = jaylink::scan_usb()?
            .filter_map(|usb_info| {
                if usb_info.vid() == selector.vendor_id && usb_info.pid() == selector.product_id {
                    let device = usb_info.open();
                    if let Some(serial_number) = selector.serial_number.as_deref() {
                        if device
                            .as_ref()
                            .map(|d| d.serial_string() == serial_number)
                            .unwrap_or(false)
                        {
                            Some(device)
                        } else {
                            None
                        }
                    } else {
                        Some(device)
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if jlinks.is_empty() {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                super::ProbeCreationError::NotFound,
            ));
        } else if jlinks.len() > 1 {
            tracing::warn!("More than one matching J-Link was found. Opening the first one.")
        }
        let jlink_handle = jlinks.pop().unwrap()?;

        // Check which protocols are supported by the J-Link.
        //
        // If the J-Link has the SELECT_IF capability, we can just ask
        // it which interfaces it supports. If it doesn't have the capabilty,
        // we assume that it justs support JTAG. In that case, we will also
        // not be able to change protocols.

        let supported_protocols: Vec<WireProtocol> =
            if jlink_handle.capabilities().contains(Capability::SelectIf) {
                let interfaces = jlink_handle.available_interfaces();

                let protocols: Vec<_> =
                    interfaces.into_iter().map(WireProtocol::try_from).collect();

                protocols
                    .iter()
                    .filter(|p| p.is_err())
                    .for_each(|protocol| {
                        if let Err(JlinkError::UnknownInterface(interface)) = protocol {
                            tracing::debug!(
                            "J-Link returned interface {:?}, which is not supported by probe-rs.",
                            interface
                        );
                        }
                    });

                // We ignore unknown protocols, the chance that this happens is pretty low,
                // and we can just work with the ones we know and support.
                protocols.into_iter().filter_map(Result::ok).collect()
            } else {
                // The J-Link cannot report which interfaces it supports, and cannot
                // switch interfaces. We assume it just supports JTAG.
                vec![WireProtocol::Jtag]
            };

        Ok(Box::new(JLink {
            handle: jlink_handle,
            swo_config: None,
            supported_protocols,
            jtag_idle_cycles: 0,
            ir_len: 0,
            protocol: None,
            current_ir_reg: 1,
            speed_khz: 0,
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
        }))
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        // try to select the interface

        let actual_protocol = self.select_interface(Some(protocol))?;

        if actual_protocol == protocol {
            self.protocol = Some(protocol);
            Ok(())
        } else {
            self.protocol = Some(actual_protocol);
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        self.protocol
    }

    fn get_name(&self) -> &'static str {
        "J-Link"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        if speed_khz == 0 || speed_khz >= 0xffff {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }

        if let Ok(speeds) = self.handle.read_speeds() {
            tracing::debug!("Supported speeds: {:?}", speeds);

            let max_speed_khz = speeds.max_speed_hz() / 1000;

            if max_speed_khz < speed_khz {
                return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
            }
        };

        if let Some(expected_speed) = SpeedConfig::khz(speed_khz as u16) {
            self.handle.set_speed(expected_speed)?;
            self.speed_khz = speed_khz;
        } else {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), super::DebugProbeError> {
        tracing::debug!("Attaching to J-Link");

        let configured_protocol = match self.protocol {
            Some(protocol) => protocol,
            None => {
                if self.supported_protocols.contains(&WireProtocol::Swd) {
                    WireProtocol::Swd
                } else {
                    // At least one protocol is always supported
                    *self.supported_protocols.first().unwrap()
                }
            }
        };

        let actual_protocol = self.select_interface(Some(configured_protocol))?;

        if actual_protocol != configured_protocol {
            tracing::warn!("Protocol {} is configured, but not supported by the probe. Using protocol {} instead", configured_protocol, actual_protocol);
        }

        tracing::debug!("Attaching with protocol '{}'", actual_protocol);
        self.protocol = Some(actual_protocol);

        // Get reference to JayLink instance
        let capabilities = self.handle.capabilities();

        // Log some information about the probe
        let serial = self.handle.serial_string().trim_start_matches('0');
        tracing::info!("J-Link: S/N: {}", serial);
        tracing::debug!("J-Link: Capabilities: {:?}", capabilities);
        let fw_version = self
            .handle
            .read_firmware_version()
            .unwrap_or_else(|_| "?".into());
        tracing::info!("J-Link: Firmware version: {}", fw_version);
        match self.handle.read_hardware_version() {
            Ok(hw_version) => tracing::info!("J-Link: Hardware version: {}", hw_version),
            Err(_) => tracing::info!("J-Link: Hardware version: ?"),
        };

        // Check and report the target voltage.
        let target_voltage = self.get_target_voltage()?.expect("The J-Link returned None when it should only be able to return Some(f32) or an error. Please report this bug!");
        if target_voltage < crate::probe::LOW_TARGET_VOLTAGE_WARNING_THRESHOLD {
            tracing::warn!(
                "J-Link: Target voltage (VTref) is {:2.2} V. Is your target device powered?",
                target_voltage
            );
        } else {
            tracing::info!("J-Link: Target voltage: {:2.2} V", target_voltage);
        }

        match actual_protocol {
            WireProtocol::Jtag => {
                // try some JTAG stuff

                tracing::debug!("Resetting JTAG chain using trst");
                self.handle.reset_trst()?;

                tracing::debug!("Resetting JTAG chain by setting tms high for 32 bits");

                // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
                let tms = vec![true, true, true, true, true, false];
                let tdi = iter::repeat(false).take(6);

                let response: Vec<_> = self.handle.jtag_io(tms, tdi)?.collect();

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
            }
            WireProtocol::Swd => {
                // Attaching is handled in sequence

                // We are ready to debug.
            }
        }

        tracing::debug!("Attached succesfully");

        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), super::DebugProbeError> {
        Err(super::DebugProbeError::NotImplemented("target_reset"))
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.handle.set_reset(false)?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.handle.set_reset(true)?;
        Ok(())
    }

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        if self.supported_protocols.contains(&WireProtocol::Jtag) {
            match RiscvCommunicationInterface::new(self) {
                Ok(interface) => Ok(interface),
                Err((probe, err)) => Err((probe.into_probe(), err)),
            }
        } else {
            Err((
                RawDapAccess::into_probe(self),
                DebugProbeError::InterfaceNotAvailable("JTAG").into(),
            ))
        }
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        Some(self as _)
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        Some(self as _)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn has_riscv_interface(&self) -> bool {
        self.supported_protocols.contains(&WireProtocol::Jtag)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        Some(self)
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        let uninitialized_interface = ArmCommunicationInterface::new(self, true);

        Ok(Box::new(uninitialized_interface))
    }

    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        // Convert the integer millivolts value from self.handle to volts as an f32.
        Ok(Some((self.handle.read_target_voltage()? as f32) / 1000f32))
    }
}

impl JTAGAccess for JLink {
    fn set_ir_len(&mut self, len: u32) {
        self.ir_len = len as usize;
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
            self.write_ir(&address_bits[..1], self.ir_len)?;
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
            self.write_ir(&address_bits[..1], self.ir_len)?;
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
}

impl RawProtocolIo for JLink {
    fn jtag_shift_tms<M>(&mut self, tms: M, tdi: bool) -> Result<(), DebugProbeError>
    where
        M: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Swd {
            panic!("Logic error, requested jtag_io when in SWD mode");
        }

        self.probe_statistics.report_io();
        let tms_iter: Vec<_> = tms.into_iter().collect();
        let count = tms_iter.len();

        self.handle
            .jtag_io(tms_iter, std::iter::repeat(tdi).take(count))?;
        Ok(())
    }

    fn jtag_shift_tdi<I>(&mut self, tms: bool, tdi: I) -> Result<(), DebugProbeError>
    where
        I: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Swd {
            panic!("Logic error, requested jtag_io when in SWD mode");
        }

        self.probe_statistics.report_io();
        let tdi_iter: Vec<_> = tdi.into_iter().collect();
        let count = tdi_iter.len();

        self.handle
            .jtag_io(std::iter::repeat(tms).take(count), tdi_iter)?;
        Ok(())
    }

    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        if self.protocol.unwrap() == crate::WireProtocol::Jtag {
            panic!("Logic error, requested swd_io when in JTAG mode");
        }

        self.probe_statistics.report_io();

        let iter = self.handle.swd_io(dir, swdio)?;

        Ok(iter.collect())
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }

    fn probe_statistics(&mut self) -> &mut ProbeStatistics {
        &mut self.probe_statistics
    }
}

impl DapProbe for JLink {}

impl SwoAccess for JLink {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        self.swo_config = Some(*config);
        self.handle
            .swo_start(SwoMode::Uart, config.baud(), SWO_BUFFER_SIZE.into())
            .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        self.swo_config = None;
        self.handle
            .swo_stop()
            .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
        Ok(())
    }

    fn swo_buffer_size(&mut self) -> Option<usize> {
        Some(SWO_BUFFER_SIZE.into())
    }

    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, ArmError> {
        let end = std::time::Instant::now() + timeout;
        let mut buf = vec![0; SWO_BUFFER_SIZE.into()];

        let poll_interval = self
            .swo_poll_interval_hint(&self.swo_config.unwrap())
            .unwrap();

        let mut bytes = vec![];
        loop {
            let data = self
                .handle
                .swo_read(&mut buf)
                .map_err(|e| ArmError::from(DebugProbeError::ProbeSpecific(Box::new(e))))?;
            bytes.extend(data.as_ref());
            let now = std::time::Instant::now();
            if now + poll_interval < end {
                std::thread::sleep(poll_interval);
            } else {
                break;
            }
        }
        Ok(bytes)
    }
}

#[tracing::instrument(skip_all)]
pub(crate) fn list_jlink_devices() -> Vec<DebugProbeInfo> {
    match jaylink::scan_usb() {
        Ok(devices) => devices
            .map(|device_info| {
                let vid = device_info.vid();
                let pid = device_info.pid();
                let (serial, product) = if let Ok(device) = device_info.open() {
                    let serial = device.serial_string();
                    let serial = if serial.is_empty() {
                        None
                    } else {
                        Some(serial.to_owned())
                    };
                    let product = device.product_string();
                    let product = if product.is_empty() {
                        None
                    } else {
                        Some(product.to_owned())
                    };
                    (serial, product)
                } else {
                    (None, None)
                };
                DebugProbeInfo::new(
                    format!(
                        "J-Link{}",
                        product.map(|p| format!(" ({p})")).unwrap_or_default()
                    ),
                    vid,
                    pid,
                    serial,
                    DebugProbeType::JLink,
                    None,
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

impl From<jaylink::Error> for DebugProbeError {
    fn from(e: jaylink::Error) -> DebugProbeError {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JlinkError {
    #[error("Unknown interface reported by J-Link: {0:?}")]
    UnknownInterface(jaylink::Interface),
}

impl TryFrom<jaylink::Interface> for WireProtocol {
    type Error = JlinkError;

    fn try_from(interface: Interface) -> Result<Self, Self::Error> {
        match interface {
            Interface::Jtag => Ok(WireProtocol::Jtag),
            Interface::Swd => Ok(WireProtocol::Swd),
            unknown_interface => Err(JlinkError::UnknownInterface(unknown_interface)),
        }
    }
}
