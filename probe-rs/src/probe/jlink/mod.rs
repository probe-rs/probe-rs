//! Support for J-Link Debug probes

use jaylink::JayLink;

use std::convert::TryInto;
use std::iter;
use std::sync::Mutex;

use crate::probe::{
    DAPAccess, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeType, WireProtocol,
};

use crate::architecture::riscv::communication_interface::JTAGAccess;
use bitfield::bitfield;
use bitvec::bitvec;
use bitvec::vec::BitVec;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct JLink {
    handle: Mutex<JayLink>,

    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    jtag_idle_cycles: Option<u8>,

    /// Currently selected protocol
    protocol: Option<WireProtocol>,

    current_ir_reg: u32,
}

impl JLink {
    fn idle_cycles(&self) -> u8 {
        self.jtag_idle_cycles.unwrap_or(0)
    }

    fn select_interface(
        &mut self,
        protocol: Option<WireProtocol>,
    ) -> Result<WireProtocol, DebugProbeError> {
        let handle = self.handle.get_mut().unwrap();

        let capabilities = handle.read_capabilities()?;

        if capabilities.contains(jaylink::Capabilities::SELECT_IF) {
            if let Some(protocol) = protocol {
                let jlink_interface = match protocol {
                    WireProtocol::Swd => jaylink::Interface::Swd,
                    WireProtocol::Jtag => jaylink::Interface::Jtag,
                };

                if handle
                    .read_available_interfaces()?
                    .find(|interface| interface == &jlink_interface)
                    .is_some()
                {
                    // We can select the desired interface
                    handle.select_interface(jlink_interface)?;
                    Ok(protocol)
                } else {
                    return Err(DebugProbeError::UnsupportedProtocol(protocol));
                }
            } else {
                // No special protocol request
                let current_protocol = handle.read_current_interface()?;

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
        log::debug!("Read {} bits from DR", register_bits);

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

        let jlink = self.handle.get_mut().unwrap();
        let mut response = jlink.jtag_io(tms, tdi)?;

        log::trace!("Response: {:?}", response);

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

        log::debug!("Read from DR: {:?}", result);

        Ok(result)
    }

    /// Write IR register with the specified data. The
    /// IR register might have an odd length, so the dta
    /// will be truncated to `len` bits. If data has less
    /// than `len` bits, an error will be returned.
    fn write_ir(&mut self, data: &[u8], len: usize) -> Result<(), DebugProbeError> {
        log::debug!("Write IR: {:?}, len={}", data, len);

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

        let skip_out = 4;

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

        tdi.extend_from_slice(&tms_enter_idle);

        log::trace!("tms: {:?}", tms);
        log::trace!("tdi: {:?}", tdi);

        let jlink = self.handle.get_mut().unwrap();
        let response = jlink.jtag_io(tms, tdi)?;

        log::trace!("Response: {:?}", response);

        assert!(
            len < 8,
            "Not yet implemented for IR registers larger than 8 bit"
        );

        self.current_ir_reg = data[0] as u32;

        // Maybe we could return the previous state of the IR register here...

        Ok(())
    }

    fn write_dr(&mut self, data: &[u8], register_bits: usize) -> Result<Vec<u8>, DebugProbeError> {
        log::debug!("Write DR: {:?}, len={}", data, register_bits);

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

        let jlink = self.handle.get_mut().unwrap();
        let mut response = jlink.jtag_io(tms, tdi)?;

        log::trace!("Response: {:?}", response);

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

        log::trace!("result: {:?}", result);

        Ok(result)
    }
}

bitfield! {
    struct Dtmcs(u32);
    impl Debug;

    dmihardreset, set_dmihardreset: 17;
    dmireset, set_dmireset: 16;
    idle, _: 14, 12;
    dmistat, _: 11,10;
    abits, _: 9,4;
    version, _: 3,0;
}

impl DebugProbe for JLink {
    fn new_from_probe_info(info: &super::DebugProbeInfo) -> Result<Box<Self>, DebugProbeError> {
        let mut usb_devices: Vec<_> = jaylink::scan_usb()?
            .filter(|usb_info| {
                usb_info.vid() == info.vendor_id && usb_info.pid() == info.product_id
            })
            .collect();

        if usb_devices.len() != 1 {
            // TODO: Add custom error
            return Err(DebugProbeError::ProbeCouldNotBeCreated);
        }

        Ok(Box::new(JLink {
            handle: Mutex::new(usb_devices.pop().unwrap().open()?),
            jtag_idle_cycles: None,
            protocol: None,
            current_ir_reg: 1,
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

    fn get_name(&self) -> &'static str {
        "J-Link"
    }

    fn attach(&mut self) -> Result<(), super::DebugProbeError> {
        // todo: check selected protocols

        self.select_protocol(self.protocol.unwrap_or(WireProtocol::Jtag))?;

        // try some JTAG stuff
        let jlink = self.handle.get_mut().unwrap();

        log::info!(
            "Target voltage: {:2.2} V",
            jlink.read_target_voltage()? as f32 / 1000f32
        );

        log::debug!("Resetting JTAG chain using trst");
        jlink.reset_trst()?;

        log::debug!("Resetting JTAG chain by setting tms high for 32 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = vec![true, true, true, true, true, false];
        let tdi = iter::repeat(false).take(6);

        let response: Vec<_> = jlink.jtag_io(tms, tdi)?.collect();

        log::debug!("Response to reset: {:?}", response);

        // try to read the idcode
        let idcode_bytes = self.read_dr(32)?;
        let idcode = u32::from_le_bytes((&idcode_bytes[..]).try_into().unwrap());

        println!("IDCODE: {:#010x}", idcode);

        // TODO:
        //
        // * select dtmcs register (ir -> 0x10)
        // * read 32 bit dtmcs value
        //
        // Sanity check: abits should be 7

        self.write_ir(&[0x10], 5)?;

        let dr_bytes = self.read_dr(32)?;
        let dtmcs_raw = u32::from_le_bytes((&dr_bytes[..]).try_into().unwrap());

        let dtmcs = Dtmcs(dtmcs_raw);

        println!("dtmcs: {:?}", dtmcs);

        self.jtag_idle_cycles = Some(dtmcs.idle() as u8);

        Ok(())
    }

    fn detach(&mut self) -> Result<(), super::DebugProbeError> {
        unimplemented!()
    }

    fn target_reset(&mut self) -> Result<(), super::DebugProbeError> {
        unimplemented!()
    }

    fn dedicated_memory_interface(&self) -> Option<crate::Memory> {
        None
    }

    fn get_interface_dap(&self) -> Option<&dyn DAPAccess> {
        None
    }

    fn get_interface_dap_mut(&mut self) -> Option<&mut dyn DAPAccess> {
        None
    }

    fn get_interface_jtag(&self) -> Option<&dyn JTAGAccess> {
        Some(self as _)
    }

    fn get_interface_jtag_mut(&mut self) -> Option<&mut dyn JTAGAccess> {
        Some(self as _)
    }
}

impl JTAGAccess for JLink {
    /// Read the data register
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        let address_bits = address.to_le_bytes();

        // TODO: This is limited to 5 bit addresses for now
        assert!(
            address <= 0x1f,
            "JTAG Register addresses are fixed to 5 bits"
        );

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
        assert!(
            address <= 0x1f,
            "JTAG Register addresses are fixed to 5 bits"
        );

        if self.current_ir_reg != address {
            // Write IR register
            self.write_ir(&address_bits[..1], 5)?;
        }

        // write DR register
        self.write_dr(data, len as usize)
    }
}

fn bits_to_byte(bits: jaylink::BitIter) -> u32 {
    let mut bit_val = 0u32;

    for (index, bit) in bits.into_iter().take(32).enumerate() {
        if bit {
            bit_val |= 1 << index;
        }
    }

    bit_val
}

#[derive(PartialEq)]
enum JtagState {
    Reset,
    Idle,
    // TODO: Add more state
}

pub(crate) fn list_jlink_devices() -> Result<impl Iterator<Item = DebugProbeInfo>, DebugProbeError>
{
    Ok(jaylink::scan_usb()?.map(|device_info| {
        DebugProbeInfo::new(
            format!(
                "J-Link (VID: {:#06x}, PID: {:#06x})",
                device_info.vid(),
                device_info.pid()
            ),
            device_info.vid(),
            device_info.pid(),
            None,
            DebugProbeType::JLink,
        )
    }))
}

impl From<jaylink::Error> for DebugProbeError {
    fn from(e: jaylink::Error) -> DebugProbeError {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}
