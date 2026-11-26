//! Native WCH-Link flash programming.
//!
//! The host uploads a flash loader to the probe, then streams the image over bulk USB.
//! Protocol reference: <https://github.com/ch32-rs/wlink>.
//!
//! ```text
//! SetWriteMemoryRegion(addr, len)
//! WriteFlashOP
//! <loader in packet_size packets>
//! Unknown07AfterFlashOPWritten       // probe answers 0x07
//! WriteFlash
//! <image in write_pack_size chunks of packet_size packets, each chunk acknowledged>
//! End
//! ```

use std::time::Duration;

use super::RiscvChip;
use super::WchLink;
use super::commands::{
    AttachChip, CheckReadProtect, CheckWriteProtect, DetachChip, Program, SetSpeed,
    SetWriteMemoryRegion, UnprotectReadFlash, UnprotectWriteFlash, WchLinkCommand,
};
use super::usb_interface::FLASH_USB_TIMEOUT;
use crate::probe::DebugProbeError;

/// Expected acknowledgement payloads.
const COMMIT_FLASH_OP_ACK: u8 = 0x07;
const FASTPROGRAM_CHUNK_ACK: u8 = 0x04;

/// Flash protection status codes (CMD 0x06).
const FLAG_READ_PROTECTED: u8 = 0x01;
const FLAG_READ_UNPROTECTED: u8 = 0x02;
const FLAG_WRITE_PROTECTED: u8 = 0x11;
const FLAG_WRITE_UNPROTECTED: u8 = 0x00;

/// Native flashing parameters for one chip family.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeFlashParams {
    /// Name of the yaml flash-algorithm entry holding the loader blob.
    pub algo_name: &'static str,
    /// USB bulk packet size for blob / image streaming.
    pub packet_size: usize,
    /// Firmware bytes streamed per fastprogram acknowledgement.
    pub write_pack_size: usize,
    /// Base address of code flash.
    pub code_flash_base: u32,
}

/// Returns the native flashing parameters for `family`, if it has a native loader.
pub(crate) fn native_flash_params(family: RiscvChip) -> Option<NativeFlashParams> {
    /// Address 0 aliases code flash on all supported families.
    const CODE_FLASH_BASE: u32 = 0x0800_0000;

    let (algo_name, packet_size, write_pack_size): (&'static str, usize, usize) = match family {
        RiscvChip::CH32V20X | RiscvChip::CH32V30X => ("ch32-v3-usr-native", 256, 4096),
        RiscvChip::CH32V317 => ("ch32-v317-usr-native", 256, 4096),
        RiscvChip::CH32V103 => ("ch32-v1-usr-native", 128, 4096),
        RiscvChip::CH32V003 | RiscvChip::CH641 => ("ch32-v0-usr-native", 64, 1024),
        RiscvChip::CH32V00X => ("ch32-v00x-usr-native", 256, 1024),
        RiscvChip::CH32X035 | RiscvChip::CH643 => ("ch32-x0-usr-native", 256, 4096),
        RiscvChip::CH32L103 => ("ch32-l1-usr-native", 256, 4096),
        RiscvChip::CH32H4 => ("ch32-h4-usr-native", 256, 4096),
        _ => return None,
    };

    Some(NativeFlashParams {
        algo_name,
        packet_size,
        write_pack_size,
        code_flash_base: CODE_FLASH_BASE,
    })
}

/// Maps an address in the zero alias to code flash.
pub(crate) fn canonical_flash_address(params: &NativeFlashParams, address: u32) -> u32 {
    if address < params.code_flash_base {
        params.code_flash_base + address
    } else {
        address
    }
}

impl WchLink {
    fn send_native<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
    ) -> Result<C::Response, DebugProbeError> {
        tracing::debug!("WCH-Link native command: {cmd:?}");
        self.device.send_command_timeout(cmd, FLASH_USB_TIMEOUT)
    }

    /// Re-attach after an operation that resets probe-side state.
    pub(crate) fn native_reattach(&mut self) -> Result<(), DebugProbeError> {
        let deadline = std::time::Instant::now() + FLASH_USB_TIMEOUT;
        loop {
            self.send_native(DetachChip)?;
            // Repeat probe initialization when returning from DMI to native commands.
            self.get_probe_info()?;
            self.send_native(SetSpeed(self.chip_family, self.speed))?;
            let resp = self.send_native(AttachChip)?;
            if resp.chip_family == self.chip_family
                && (self.chip_id == 0 || resp.chip_id == self.chip_id)
            {
                self.chip_id = resp.chip_id;
                return Ok(());
            }
            tracing::debug!(
                "WCH-Link: waiting for chip identity, got {:#010x}",
                resp.chip_id
            );
            if std::time::Instant::now() >= deadline {
                return Err(DebugProbeError::Other(
                    "WCH-Link target identity did not recover".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Erase the whole code flash via the probe firmware.
    pub(crate) fn native_erase_flash(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("WCH-Link: native chip erase");
        self.send_native(Program::EraseFlash)?;
        self.native_reattach()
    }

    /// Program `data` to flash at `address` via the probe firmware fastprogram flow.
    ///
    /// `blob` is the flash loader of the chip family.
    pub(crate) fn native_write_flash(
        &mut self,
        address: u32,
        data: &[u8],
        blob: &[u8],
        params: &NativeFlashParams,
    ) -> Result<(), DebugProbeError> {
        if data.is_empty() {
            return Ok(());
        }
        let len = u32::try_from(data.len()).map_err(|_| {
            DebugProbeError::Other(format!(
                "WCH-Link native flashing does not support images larger than 4 GiB (got {} bytes)",
                data.len(),
            ))
        })?;

        tracing::debug!(
            "WCH-Link: native program of {} bytes at 0x{:08x} (packet {}, pack {})",
            data.len(),
            address,
            params.packet_size,
            params.write_pack_size
        );

        // Without a fresh attach, the probe ignores a second write.
        self.native_reattach()?;
        // Announce the image region.
        self.send_native(SetWriteMemoryRegion {
            start_addr: address,
            len,
        })?;

        // Everything from here runs inside an unfinished probe-side
        // transaction; on failure, attempt to leave the probe in a clean
        // state before reporting the original error.
        let result = self.write_flash_transaction(data, blob, params);
        if result.is_err() {
            self.abort_native_transaction();
        }
        result
    }

    /// Best-effort cleanup after a failed native transaction: end programming
    /// mode and reattach, so later operations do not meet probe firmware
    /// still waiting for data. Never masks the original error.
    fn abort_native_transaction(&mut self) {
        if self.send_native(Program::End).is_err() {
            tracing::warn!("WCH-Link: failed to end aborted native transaction");
        }
        if let Err(error) = self.native_reattach() {
            tracing::warn!("WCH-Link: failed to reattach after aborted transaction: {error}");
        }
    }

    fn write_flash_transaction(
        &mut self,
        data: &[u8],
        blob: &[u8],
        params: &NativeFlashParams,
    ) -> Result<(), DebugProbeError> {
        // Upload the flash loader.
        self.send_native(Program::WriteFlashOP)?;
        for chunk in blob.chunks(params.packet_size) {
            let mut packet = chunk.to_vec();
            packet.resize(params.packet_size, 0xFF);
            self.device.write_data_bulk(&packet)?;
        }
        tracing::debug!("WCH-Link: flash loader blob uploaded");

        // Commit handshake.
        let ack: u8 = self.send_native(Program::Unknown07AfterFlashOPWritten)?;
        if ack != COMMIT_FLASH_OP_ACK {
            return Err(DebugProbeError::Other(format!(
                "WCH-Link loader commit failed: expected 0x{COMMIT_FLASH_OP_ACK:02x}, got 0x{ack:02x}"
            )));
        }

        // Stream the image: `write_pack_size` bytes per 4-byte acknowledgement,
        // `packet_size` bytes per bulk packet.
        self.send_native(Program::WriteFlash)?;
        for chunk in data.chunks(params.write_pack_size) {
            for packet in chunk.chunks(params.packet_size) {
                let mut padded = packet.to_vec();
                padded.resize(params.packet_size, 0xFF);
                self.device.write_data_bulk(&padded)?;
            }
            let ack = self.device.read_data_bulk(4)?;
            if ack[3] != FASTPROGRAM_CHUNK_ACK {
                return Err(DebugProbeError::Other(format!(
                    "WCH-Link fastprogram chunk failed: ack {ack:02x?}"
                )));
            }
        }
        tracing::debug!("WCH-Link: fastprogram done");

        // Finish programming.
        let _code: u8 = self.send_native(Program::End)?;
        Ok(())
    }

    /// Disable flash read and write protection.
    ///
    /// Protection is only touched when enabled: unprotecting mass-erases the option bytes.
    pub(crate) fn native_unprotect_flash(&mut self) -> Result<(), DebugProbeError> {
        if !self.chip_family.support_flash_protect() {
            return Ok(());
        }

        // Protection queries require a fresh attach.
        self.native_reattach()?;

        let read_protect: u8 = self.send_native(CheckReadProtect)?;
        if read_protect == FLAG_READ_PROTECTED {
            tracing::info!("WCH-Link: flash is read protected, unprotecting");
            self.send_native(UnprotectReadFlash)?;
            self.native_reattach()?;
            let read_protect: u8 = self.send_native(CheckReadProtect)?;
            if read_protect != FLAG_READ_UNPROTECTED {
                return Err(DebugProbeError::Other(format!(
                    "WCH-Link failed to disable flash read protection (status 0x{read_protect:02x})"
                )));
            }
        } else if read_protect != FLAG_READ_UNPROTECTED {
            tracing::warn!("WCH-Link: unknown read protect status: 0x{read_protect:02x}");
        }

        let write_protect: u8 = self.send_native(CheckWriteProtect)?;
        if write_protect == FLAG_WRITE_PROTECTED {
            tracing::info!("WCH-Link: flash is write protected, unprotecting");
            self.send_native(UnprotectWriteFlash(0xff))?;
            self.native_reattach()?;
            let write_protect: u8 = self.send_native(CheckWriteProtect)?;
            if write_protect != FLAG_WRITE_UNPROTECTED {
                return Err(DebugProbeError::Other(format!(
                    "WCH-Link failed to disable flash write protection (status 0x{write_protect:02x})"
                )));
            }
        } else if write_protect != FLAG_WRITE_UNPROTECTED {
            tracing::warn!("WCH-Link: unknown write protect status: 0x{write_protect:02x}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::Speed;
    use super::super::usb_interface::{
        DATA_ENDPOINT_IN, DATA_ENDPOINT_OUT, ENDPOINT_IN, ENDPOINT_OUT, ScriptedUsb, UsbScriptStep,
        WchLinkUsbDevice,
    };
    use super::super::{RiscvChip, WchLink, WchLinkVariant};
    use super::*;

    fn cmd_req(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x81, cmd, payload.len() as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn cmd_resp(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x82, cmd, payload.len() as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn wr(endpoint: u8, payload: Vec<u8>) -> UsbScriptStep {
        let written = payload.len();
        UsbScriptStep::Write {
            endpoint,
            expect: payload,
            written,
        }
    }

    fn rd(endpoint: u8, payload: Vec<u8>) -> UsbScriptStep {
        UsbScriptStep::Read {
            endpoint,
            reply: Ok(payload),
        }
    }

    fn be(value: u32) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn padded(byte: u8, len: usize, packet: usize) -> Vec<u8> {
        let mut out = vec![byte; len.min(packet)];
        out.resize(packet, 0xFF);
        out
    }

    /// Refresh a CH32V30X connection at high speed.
    fn reattach_steps() -> Vec<UsbScriptStep> {
        vec![
            wr(ENDPOINT_OUT, cmd_req(0x0d, &[0xff])),
            rd(ENDPOINT_IN, cmd_resp(0x0d, &[])),
            wr(ENDPOINT_OUT, cmd_req(0x0d, &[0x01])),
            rd(ENDPOINT_IN, cmd_resp(0x0d, &[0x02, 0x0f, 0x02])),
            wr(ENDPOINT_OUT, cmd_req(0x0c, &[0x06, 0x01])),
            rd(ENDPOINT_IN, cmd_resp(0x0c, &[0x01])),
            wr(ENDPOINT_OUT, cmd_req(0x0d, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x0d, &[0x06, 0x30, 0x70, 0x05, 0x18])),
        ]
    }

    fn test_link(script: &ScriptedUsb) -> WchLink {
        WchLink {
            device: WchLinkUsbDevice::for_test(Box::new(script.clone()), true),
            name: String::from("test-link"),
            variant: WchLinkVariant::ECh32v305,
            v_major: 2,
            v_minor: 15,
            chip_id: 0,
            chip_family: RiscvChip::CH32V30X,
            last_dmi_read: None,
            use_rst_pin: false,
            speed: Speed::default(),
        }
    }

    fn v30x_params() -> NativeFlashParams {
        native_flash_params(RiscvChip::CH32V30X).expect("params must exist")
    }

    #[test]
    fn write_flash_pins_full_sequence_and_padding() {
        let blob = vec![0xBBu8; 10];
        let data = vec![0xAAu8; 300];
        let params = v30x_params();
        let mut address = be(0x0800_0000);
        address.extend_from_slice(&be(300));

        let mut steps = reattach_steps();
        steps.extend([
            wr(ENDPOINT_OUT, cmd_req(0x01, &address)),
            rd(ENDPOINT_IN, cmd_resp(0x01, &[])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x05])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x05])),
            wr(DATA_ENDPOINT_OUT, padded(0xBB, 10, 256)),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x07])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x07])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x02])),
            // Two packets share one pack acknowledgement.
            wr(DATA_ENDPOINT_OUT, padded(0xAA, 256, 256)),
            wr(DATA_ENDPOINT_OUT, padded(0xAA, 44, 256)),
            rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x04]),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x08])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x08])),
        ]);
        let script = ScriptedUsb::new(steps);

        let mut link = test_link(&script);
        link.native_write_flash(0x0800_0000, &data, &blob, &params)
            .expect("write must succeed");
        script.assert_finished();
    }

    #[test]
    fn second_pack_ack_failure_aborts_and_cleans_up() {
        // Hardware returned [41, 01, 01, 05] after accepting the first 4 KiB pack.
        let blob = vec![0xBBu8; 10];
        let data = vec![0xAAu8; 4096 + 256];
        let params = v30x_params();
        let mut address = be(0x0800_0000);
        address.extend_from_slice(&be(4096 + 256));

        let mut steps = reattach_steps();
        steps.extend([
            wr(ENDPOINT_OUT, cmd_req(0x01, &address)),
            rd(ENDPOINT_IN, cmd_resp(0x01, &[])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x05])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x05])),
            wr(DATA_ENDPOINT_OUT, padded(0xBB, 10, 256)),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x07])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x07])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x02])),
        ]);
        for _ in 0..16 {
            steps.push(wr(DATA_ENDPOINT_OUT, vec![0xAAu8; 256]));
        }
        steps.push(rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x04]));
        steps.push(wr(DATA_ENDPOINT_OUT, vec![0xAAu8; 256]));
        steps.push(rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x05]));
        steps.push(wr(ENDPOINT_OUT, cmd_req(0x02, &[0x08])));
        steps.push(rd(ENDPOINT_IN, cmd_resp(0x02, &[0x08])));
        steps.extend(reattach_steps());
        let script = ScriptedUsb::new(steps);

        let mut link = test_link(&script);
        let error = link
            .native_write_flash(0x0800_0000, &data, &blob, &params)
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("fastprogram chunk failed"),
            "unexpected error: {error:?}"
        );
        script.assert_finished();
    }
}
