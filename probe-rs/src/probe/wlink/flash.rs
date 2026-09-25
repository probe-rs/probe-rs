//! Native WCH-Link flash programming.
//!
//! Instead of driving the target through DMI and a RAM-resident flash
//! algorithm (which costs one USB round trip per DMI operation), these routines
//! let the probe firmware sequence the programming while the host streams the
//! image in bulk packets. The actual flash driving is done by a small
//! target-side loader blob, stored as `instructions` in the target's yaml
//! `*-usr-native` flash algorithm entry and invoked per chunk by the probe.
//! This mirrors the reference `wlink` tool (<https://github.com/ch32-rs/wlink>,
//! MIT/Apache-2.0):
//!
//! ```text
//! SetWriteMemoryRegion(addr, len)   // announce the image
//! WriteFlashOP                       // prepare loader upload
//! <stream family loader blob>        // e.g. 446 bytes for CH32V30X
//! Unknown07 handshake                // probe answers 0x07
//! WriteFlash                         // start fastprogram
//! <stream image in write_pack_size   // 4 KiB per 4-byte ack
//!   chunks, data_packet_size
//!   bulk packets each>
//! End                                // finish programming
//! ```
//!
//! A 128 KiB image needs ~550 bulk transfers this way, versus ~140k USB round
//! trips through the generic DMI path.

use std::time::Duration;

use super::RiscvChip;
use super::WchLink;
use super::commands::{
    AttachChip, CheckReadProtect, CheckWriteProtect, DetachChip, Program, SetReadMemoryRegion,
    SetSpeed, SetWriteMemoryRegion, UnprotectReadFlash, UnprotectWriteFlash, WchLinkCommand,
};
use crate::probe::DebugProbeError;

/// USB timeout for native flash commands.
///
/// Whole-chip erases and bulk handshakes can take seconds, far beyond the
/// 100 ms DMI command timeout. Matches the 5 s timeout `wlink` uses.
const NATIVE_CMD_TIMEOUT: Duration = Duration::from_millis(5000);

/// Expected acknowledgement payloads. See `wlink`'s `write_flash`.
const COMMIT_FLASH_OP_ACK: u8 = 0x07;
const FASTPROGRAM_CHUNK_ACK: u8 = 0x04;

/// Flash protection status codes (CMD 0x06). See `wlink`'s `ConfigChip`.
const FLAG_READ_PROTECTED: u8 = 0x01;
const FLAG_READ_UNPROTECTED: u8 = 0x02;
const FLAG_WRITE_PROTECTED: u8 = 0x11;
const FLAG_WRITE_UNPROTECTED: u8 = 0x00;

/// Native flashing parameters for one chip family.
///
/// Packet sizes mirror `wlink`'s `RiscvChip::data_packet_size` /
/// `RiscvChip::write_pack_size`; they are probe-protocol parameters, like TI's
/// SACI constants. The loader blob bytes themselves live in the target yaml's
/// `*-usr-native` flash algorithm entry and are resolved at runtime.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeFlashParams {
    /// Name of the yaml flash-algorithm entry holding the loader blob.
    pub algo_name: &'static str,
    /// USB bulk packet size for blob / image streaming.
    pub packet_size: usize,
    /// Firmware bytes streamed per fastprogram acknowledgement.
    pub write_pack_size: usize,
    /// Base address of code flash (e.g. `0x0800_0000`).
    ///
    /// Images linked at the zero alias (e.g. `0x0`) are relocated onto this
    /// base, mirroring `wlink`'s `fix_code_flash_start`.
    pub code_flash_base: u32,
}

/// Return the native flashing parameters for `family`, or `None` when the
/// family has no native loader entry (those chips keep using the generic
/// target-side flash algorithms).
pub(crate) fn native_flash_params(family: RiscvChip) -> Option<NativeFlashParams> {
    /// All natively supported families map code flash at `0x0800_0000`
    /// (the zero page is an alias); families with a zero-based code flash
    /// have no native loader entries.
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

/// Map a layout address to the canonical address the probe firmware expects.
///
/// Images may link code flash at the zero alias (`0x0`, as `wlink` accepts
/// and the `USR_LEGACY` region describes); the probe only programs the
/// canonical window at `code_flash_base`. Addresses already at or above the
/// base (code flash, system flash, option bytes) pass through unchanged.
/// Equivalent to `wlink`'s `fix_code_flash_start` over all realistic inputs.
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
        self.device.send_command_timeout(cmd, NATIVE_CMD_TIMEOUT)
    }

    /// Re-attach to the target after an operation that resets probe-side state.
    ///
    /// Mirrors `wlink`'s `reattach_chip` (detach + set speed + attach), which is
    /// required after chip erase and before protection queries.
    pub(crate) fn native_reattach(&mut self) -> Result<(), DebugProbeError> {
        self.send_native(DetachChip)?;
        self.send_native(SetSpeed(self.chip_family, self.speed))?;
        let resp = self.send_native(AttachChip)?;
        self.chip_family = resp.chip_family;
        self.chip_id = resp.chip_id;
        tracing::debug!(
            "WCH-Link re-attached to {:?} (chip id 0x{:08x})",
            self.chip_family,
            self.chip_id
        );
        Ok(())
    }

    /// Erase the whole code flash via the probe firmware.
    pub(crate) fn native_erase_flash(&mut self) -> Result<(), DebugProbeError> {
        tracing::info!("WCH-Link: native chip erase");
        let _code: u8 = self.send_native(Program::EraseFlash)?;
        // The erase resets probe-side target state; re-attach like `wlink` does.
        self.native_reattach()?;
        Ok(())
    }

    /// Program `data` to flash at `address` via the probe firmware fastprogram flow.
    ///
    /// `blob` is the family loader blob (the yaml `*-usr-native` entry's
    /// `instructions`); `params` carries the matching protocol packet sizes.
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

        // Announce the image region (`wlink_ready_write`).
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
        // Upload the family loader blob (`wlink_ramcodewrite`).
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

        // Stream the image (`wlink_fastprogram`): `write_pack_size` bytes per
        // 4-byte acknowledgement, `packet_size` bytes per bulk packet.
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

        // Finish programming (`wlink_endprogram`).
        let _code: u8 = self.send_native(Program::End)?;
        Ok(())
    }

    /// Read `len` bytes of target memory via the probe firmware.
    ///
    /// The probe streams big-endian words; they are converted back to
    /// little-endian byte order like `wlink`'s `read_memory` does.
    pub(crate) fn native_read_memory(
        &mut self,
        address: u32,
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let aligned_len = len.div_ceil(4).checked_mul(4).ok_or_else(|| {
            DebugProbeError::Other(format!(
                "WCH-Link native read of {len} bytes cannot be represented"
            ))
        })?;
        self.send_native(SetReadMemoryRegion {
            start_addr: address,
            len: aligned_len,
        })?;
        self.send_native(Program::ReadMemory)?;

        let mut mem = self.device.read_data_bulk(aligned_len as usize)?;
        let (words, _) = mem.as_chunks_mut::<4>();
        for word in words {
            word.reverse();
        }
        mem.truncate(len as usize);
        Ok(mem)
    }

    /// Disable flash read/write protection, mirroring `wlink`'s unprotect flow.
    ///
    /// Protection is only touched when actually enabled: an unnecessary
    /// unprotect would mass-erase the option-byte page.
    pub(crate) fn native_unprotect_flash(&mut self) -> Result<(), DebugProbeError> {
        if !self.chip_family.support_flash_protect() {
            return Ok(());
        }

        // Like `wlink`, protection queries require a fresh attach.
        self.native_reattach()?;

        // A failed unprotect must fail the flash, not limp on into an
        // erase followed by a predictable programming failure. Unknown
        // statuses are only warned about (matching `wlink`): the chip may
        // still be programmable, and unprotecting on unknown state could
        // needlessly mass-erase the option bytes.
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
    use super::*;

    #[test]
    fn family_params_match_wlink() {
        // (family, yaml entry name, packet size, write pack size). Packet
        // sizes mirror `wlink`'s `data_packet_size` and `write_pack_size`;
        // blob bytes live in the yaml entries (pinned by the registry test
        // in `vendor::wch::sequences`).
        let cases = [
            (RiscvChip::CH32V20X, "ch32-v3-usr-native", 256, 4096),
            (RiscvChip::CH32V30X, "ch32-v3-usr-native", 256, 4096),
            (RiscvChip::CH32V317, "ch32-v317-usr-native", 256, 4096),
            (RiscvChip::CH32V103, "ch32-v1-usr-native", 128, 4096),
            (RiscvChip::CH32V003, "ch32-v0-usr-native", 64, 1024),
            (RiscvChip::CH641, "ch32-v0-usr-native", 64, 1024),
            (RiscvChip::CH32V00X, "ch32-v00x-usr-native", 256, 1024),
            (RiscvChip::CH32X035, "ch32-x0-usr-native", 256, 4096),
            (RiscvChip::CH643, "ch32-x0-usr-native", 256, 4096),
            (RiscvChip::CH32L103, "ch32-l1-usr-native", 256, 4096),
            (RiscvChip::CH32H4, "ch32-h4-usr-native", 256, 4096),
        ];
        for (family, algo_name, packet_size, write_pack_size) in cases {
            let params = native_flash_params(family)
                .unwrap_or_else(|| panic!("{family:?} should have native params"));
            assert_eq!(params.algo_name, algo_name, "{family:?} entry name");
            assert_eq!(params.packet_size, packet_size, "{family:?} packet size");
            assert_eq!(
                params.write_pack_size, write_pack_size,
                "{family:?} write pack size"
            );
            assert!(
                params.write_pack_size.is_multiple_of(params.packet_size),
                "{family:?}: write packs must be a multiple of the packet size"
            );
        }
    }

    #[test]
    fn canonical_addresses_match_wlink() {
        use super::canonical_flash_address;

        let params = native_flash_params(RiscvChip::CH32V30X).unwrap();
        // Zero-alias images relocate onto the code flash base.
        assert_eq!(canonical_flash_address(&params, 0x0000_0000), 0x0800_0000);
        assert_eq!(canonical_flash_address(&params, 0x0000_1234), 0x0800_1234);
        // Canonical, system flash and option bytes pass through unchanged.
        assert_eq!(canonical_flash_address(&params, 0x0800_0000), 0x0800_0000);
        assert_eq!(canonical_flash_address(&params, 0x0801_2345), 0x0801_2345);
        assert_eq!(canonical_flash_address(&params, 0x1fff_8000), 0x1fff_8000);
        assert_eq!(canonical_flash_address(&params, 0x1fff_f800), 0x1fff_f800);
    }

    #[test]
    fn families_without_blobs_fall_back() {
        for family in [
            RiscvChip::CH57X,
            RiscvChip::CH56X,
            RiscvChip::CH32F10X,
            RiscvChip::CH58X,
            RiscvChip::CH8571,
            RiscvChip::CH59X,
            RiscvChip::CH570,
        ] {
            assert!(
                native_flash_params(family).is_none(),
                "{family:?} must fall back to generic flashing"
            );
        }
    }
}

/// Scripted protocol tests: replay exact USB traffic (including failure
/// acknowledgements observed on hardware) and assert sequencing, padding,
/// error propagation, and cleanup.
#[cfg(test)]
mod scripted_tests {
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

    /// Detach + set speed + attach, as `native_reattach` performs it for a
    /// CH32V30X chip at high speed.
    fn reattach_steps() -> Vec<UsbScriptStep> {
        vec![
            wr(ENDPOINT_OUT, cmd_req(0x0d, &[0xff])),
            rd(ENDPOINT_IN, cmd_resp(0x0d, &[])),
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

        let script = ScriptedUsb::new(vec![
            // Announce region.
            wr(ENDPOINT_OUT, cmd_req(0x01, &address)),
            rd(ENDPOINT_IN, cmd_resp(0x01, &[])),
            // Prepare loader upload.
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x05])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x05])),
            // Blob, padded to the packet size.
            wr(DATA_ENDPOINT_OUT, padded(0xBB, 10, 256)),
            // Commit handshake.
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x07])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x07])),
            // Start fastprogram.
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x02])),
            // One 4 KiB pack: two padded packets, one ack.
            wr(DATA_ENDPOINT_OUT, padded(0xAA, 256, 256)),
            wr(DATA_ENDPOINT_OUT, padded(0xAA, 44, 256)),
            rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x04]),
            // End programming.
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x08])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x08])),
        ]);

        let mut link = test_link(&script);
        link.native_write_flash(0x0800_0000, &data, &blob, &params)
            .expect("write must succeed");
        script.assert_finished();
    }

    #[test]
    fn second_pack_ack_failure_aborts_and_cleans_up() {
        // Reproduces the ack [41, 01, 01, 05] observed on hardware: the
        // first 4 KiB pack is accepted, the second pack's ack fails.
        let blob = vec![0xBBu8; 10];
        let data = vec![0xAAu8; 4096 + 256];
        let params = v30x_params();
        let mut address = be(0x0800_0000);
        address.extend_from_slice(&be(4096 + 256));

        let mut steps = vec![
            wr(ENDPOINT_OUT, cmd_req(0x01, &address)),
            rd(ENDPOINT_IN, cmd_resp(0x01, &[])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x05])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x05])),
            wr(DATA_ENDPOINT_OUT, padded(0xBB, 10, 256)),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x07])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x07])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x02])),
        ];
        for _ in 0..16 {
            steps.push(wr(DATA_ENDPOINT_OUT, vec![0xAAu8; 256]));
        }
        steps.push(rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x04]));
        steps.push(wr(DATA_ENDPOINT_OUT, vec![0xAAu8; 256]));
        steps.push(rd(DATA_ENDPOINT_IN, vec![0x41, 0x01, 0x01, 0x05]));
        // Cleanup: End, then reattach.
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

    #[test]
    fn unprotect_still_protected_is_an_error() {
        let mut steps = reattach_steps();
        steps.extend([
            wr(ENDPOINT_OUT, cmd_req(0x06, &[0x01])),
            rd(ENDPOINT_IN, cmd_resp(0x06, &[0x01])),
            wr(ENDPOINT_OUT, cmd_req(0x06, &[0x02])),
            rd(ENDPOINT_IN, cmd_resp(0x06, &[])),
        ]);
        steps.extend(reattach_steps());
        // Still protected after unprotecting: must fail, and must not
        // proceed to the write-protection query.
        steps.extend([
            wr(ENDPOINT_OUT, cmd_req(0x06, &[0x01])),
            rd(ENDPOINT_IN, cmd_resp(0x06, &[0x01])),
        ]);
        let script = ScriptedUsb::new(steps);

        let mut link = test_link(&script);
        let error = link.native_unprotect_flash().unwrap_err();
        assert!(
            format!("{error:?}").contains("failed to disable flash read protection"),
            "unexpected error: {error:?}"
        );
        script.assert_finished();
    }

    #[test]
    fn read_memory_fixes_endianness_and_truncates() {
        let mut address = be(0x0800_0000);
        address.extend_from_slice(&be(8));
        let script = ScriptedUsb::new(vec![
            wr(ENDPOINT_OUT, cmd_req(0x03, &address)),
            rd(ENDPOINT_IN, cmd_resp(0x03, &[])),
            wr(ENDPOINT_OUT, cmd_req(0x02, &[0x0c])),
            rd(ENDPOINT_IN, cmd_resp(0x02, &[0x0c])),
            rd(
                DATA_ENDPOINT_IN,
                vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            ),
        ]);

        let mut link = test_link(&script);
        let memory = link
            .native_read_memory(0x0800_0000, 6)
            .expect("must succeed");
        assert_eq!(memory, vec![0x04, 0x03, 0x02, 0x01, 0x08, 0x07]);
        script.assert_finished();
    }
}
