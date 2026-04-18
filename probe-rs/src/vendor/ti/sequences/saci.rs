//! SACI (Secure API for CC devices) protocol support.
//!
//! Contains register definitions, command IDs, result codes, and transport
//! functions for communicating with the ROM SACI handler via the SEC-AP.
//! All transport functions are stateless free functions; callers supply the
//! DapAccess interface on every call.
use bitfield::bitfield;
use std::thread;
use std::time::{Duration, Instant};

use crate::architecture::arm::DapAccess;
use crate::architecture::arm::{ArmError, FullyQualifiedApAddress};

// ---- Access port selection --------------------------------------------------

/// Access Port Select values for CC23xx/CC27xx devices.
#[derive(Debug, Clone, Copy)]
pub enum ApSel {
    /// Config-AP: read device type and status information.
    CfgAp = 1,
    /// Sec-AP: send and receive SACI commands.
    SecAp = 2,
}

impl From<ApSel> for FullyQualifiedApAddress {
    fn from(apsel: ApSel) -> Self {
        FullyQualifiedApAddress::v1_with_default_dp(apsel as u8)
    }
}

// ---- Register definitions ---------------------------------------------------

bitfield! {
    /// Device Status Register, part of CFG-AP.
    ///
    /// Reflects the current device and boot state.
    #[derive(Copy, Clone)]
    pub struct DeviceStatusRegister(u32);
    impl Debug;
    /// Indicates whether the AHB-AP is accessible.
    ///
    /// 0: device is in SACI mode, AHB-AP not accessible.
    /// 1: device is not in SACI mode, AHB-AP accessible.
    pub ahb_ap_available, _: 24;
    /// Boot status field.
    pub u8, boot_status, _: 15, 8;
}

// Address of the device status register within the CFG-AP.
const DEVICE_STATUS_ADDRESS: u64 = 0x0C;

/// Boot status value indicating the device is halted waiting for a debugger.
pub const BOOT_STATUS_APP_WAITLOOP_DBGPROBE: u8 = 0xC1;
/// Boot status value indicating the bootloader is halted waiting for a debugger.
pub const BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE: u8 = 0x81;
/// Boot status value indicating the ROM boot is halted waiting for a debugger.
pub const BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE: u8 = 0x38;

bitfield! {
    /// TX_CTRL Register, part of SEC-AP.
    ///
    /// Controls transmission of SACI command words to the device.
    #[derive(Copy, Clone)]
    struct TxCtrlRegister(u32);
    impl Debug;
    /// TX data register is full; wait before writing another word.
    ///
    /// 0: TXD ready for a new word.
    /// 1: TXD full, poll until clear.
    pub txd_full, _: 0;
    /// Command Start bit. Must be set before the first word of each new command.
    pub cmd_start, set_cmd_start: 1;
}

impl TxCtrlRegister {
    fn read(interface: &mut dyn DapAccess) -> Result<Self, ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
        let val = interface.read_raw_ap_register(&sec_ap, regs::TX_CTRL)?;
        Ok(Self(val))
    }

    fn write(&self, interface: &mut dyn DapAccess) -> Result<(), ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
        interface.write_raw_ap_register(&sec_ap, regs::TX_CTRL, self.0)
    }
}

bitfield! {
    /// RX_CTRL Register, part of SEC-AP.
    ///
    /// Indicates whether a response word is available from the device.
    #[derive(Copy, Clone)]
    struct RxCtrlRegister(u32);
    impl Debug;
    /// Response word is ready.
    ///
    /// 0: RXD empty.
    /// 1: RXD has a word ready to read.
    pub rxd_ready, _: 0;
}

impl RxCtrlRegister {
    fn read(interface: &mut dyn DapAccess) -> Result<Self, ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
        let val = interface.read_raw_ap_register(&sec_ap, regs::RX_CTRL)?;
        Ok(Self(val))
    }
}

// ---- Command IDs ------------------------------------------------------------

/// SACI command IDs for flash and debug operations.
///
/// Values verified against the CC23xx/CC27xx TRM and the OpenOCD reference
/// implementation (cc_lpf3_flash.h).
#[allow(dead_code)]
pub mod cmd {
    /// Magic key required as the second word of every flash command.
    pub const FLASH_KEY: u32 = 0xB7E3A08F;
    /// Exit SACI mode and halt at the first instruction.
    pub const DEBUG_EXIT_SACI_HALT: u32 = 0x07;
    /// Exit SACI mode and resume application execution.
    pub const BLDR_APP_EXIT_SACI_RUN: u32 = 0x15;
    /// Erase the entire chip (MAIN + CCFG + SCFG for CC27xx).
    pub const FLASH_ERASE_CHIP: u32 = 0x09;
    /// Program a single MAIN sector (non-pipelined, within one sector).
    pub const FLASH_PROG_MAIN_SECTOR: u32 = 0x0E;
    /// Program MAIN flash sectors using the pipelined protocol.
    pub const FLASH_PROG_MAIN_PIPELINED: u32 = 0x0F;
    /// Program the CCFG sector (always a full 512 words).
    pub const FLASH_PROG_CCFG_SECTOR: u32 = 0x0C;
    /// Verify MAIN flash sectors using CRC32.
    pub const FLASH_VERIFY_MAIN_SECTORS: u32 = 0x10;
    /// Verify the CCFG sector.
    pub const FLASH_VERIFY_CCFG_SECTOR: u32 = 0x11;
    /// Program the SCFG sector (CC27xx only).
    pub const FLASH_PROG_SCFG_SECTOR: u32 = 0x1A;
    /// Verify the SCFG sector (CC27xx only).
    pub const FLASH_VERIFY_SCFG_SECTOR: u32 = 0x1B;
    /// No operation.
    pub const MISC_NO_OPERATION: u32 = 0x01;
}

// ---- SEC-AP register offsets ------------------------------------------------

/// SEC-AP register offsets used by the SACI transport layer.
#[allow(dead_code)]
pub mod regs {
    /// TX_DATA: write command/data words here.
    pub const TX_DATA: u64 = 0x00;
    /// TX_CTRL: control command transmission.
    pub const TX_CTRL: u64 = 0x04;
    /// RX_DATA: read response words from here.
    pub const RX_DATA: u64 = 0x08;
    /// RX_CTRL: check whether a response word is ready.
    pub const RX_CTRL: u64 = 0x0C;
}

// ---- Result codes -----------------------------------------------------------

/// SACI command result codes.
///
/// Success is 0x00; all error codes occupy the 0x80-0xFF range.
/// Values verified against the CC23xx/CC27xx TRM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SaciResult {
    /// Command completed successfully.
    Success = 0x00,
    /// Unknown or unsupported command ID.
    InvalidCmdId = 0x80,
    /// Invalid address parameter.
    InvalidAddressParam = 0x81,
    /// Invalid size parameter.
    InvalidSizeParam = 0x82,
    /// Invalid or missing flash key (FLASH_KEY mismatch).
    InvalidKeyParam = 0x83,
    /// Flash FSM hardware error.
    FlashFsmError = 0x84,
    /// Too many parameter words (buffer overflow).
    ParamBufferOverflow = 0x85,
    /// Command not allowed in the current device state.
    NotAllowed = 0x86,
    /// CRC32 mismatch during verification.
    Crc32Mismatch = 0x87,
    /// Blank check failed (flash not erased).
    BlankCheckFailed = 0x89,
    /// Unrecognised result code.
    Unknown = 0xFF,
}

impl From<u8> for SaciResult {
    fn from(value: u8) -> Self {
        match value {
            0x00 => SaciResult::Success,
            0x80 => SaciResult::InvalidCmdId,
            0x81 => SaciResult::InvalidAddressParam,
            0x82 => SaciResult::InvalidSizeParam,
            0x83 => SaciResult::InvalidKeyParam,
            0x84 => SaciResult::FlashFsmError,
            0x85 => SaciResult::ParamBufferOverflow,
            0x86 => SaciResult::NotAllowed,
            0x87 => SaciResult::Crc32Mismatch,
            0x89 => SaciResult::BlankCheckFailed,
            _ => SaciResult::Unknown,
        }
    }
}

// ---- Transport functions ----------------------------------------------------

/// Poll TX_CTRL until the TX buffer is ready or the timeout elapses.
pub fn poll_tx_ctrl(interface: &mut dyn DapAccess, timeout: Duration) -> Result<(), ArmError> {
    let start = Instant::now();
    loop {
        if !TxCtrlRegister::read(interface)?.txd_full() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(ArmError::Timeout);
        }
    }
}

/// Poll RX_CTRL until a response word is available or the timeout elapses.
pub fn poll_rx_ctrl(interface: &mut dyn DapAccess, timeout: Duration) -> Result<(), ArmError> {
    let start = Instant::now();
    loop {
        if RxCtrlRegister::read(interface)?.rxd_ready() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(ArmError::Timeout);
        }
        thread::sleep(Duration::from_micros(5));
    }
}

/// Send a single-word SACI command (e.g. DEBUG_EXIT_SACI_HALT).
pub fn send_command(interface: &mut dyn DapAccess, command: u32) -> Result<(), ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();

    poll_tx_ctrl(interface, Duration::from_millis(100))?;

    let mut tx_ctrl = TxCtrlRegister(0);
    tx_ctrl.set_cmd_start(true);
    tx_ctrl.write(interface)?;

    interface.write_raw_ap_register(&sec_ap, regs::TX_DATA, command)?;

    poll_tx_ctrl(interface, Duration::from_millis(100))?;

    Ok(())
}

/// Send a multi-word SACI command sequence.
///
/// Follows the OpenOCD cc_lpf3_saci_send_cmd protocol:
///   1. Poll TXD_FULL clear.
///   2. Set CMD_START in TX_CTRL, write words[0].
///   3. Clear CMD_START.
///   4. For each subsequent word: poll TXD_FULL clear, write word.
///   5. Final poll to confirm the last word was consumed.
pub fn send_words(
    interface: &mut dyn DapAccess,
    words: &[u32],
    timeout: Duration,
) -> Result<(), ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();

    if words.is_empty() {
        return Ok(());
    }

    // Poll until the TX buffer is ready for a new command.
    poll_tx_ctrl(interface, timeout)?;

    // Set CMD_START to open a new command frame, then send the first word.
    let mut tx_ctrl = TxCtrlRegister(0);
    tx_ctrl.set_cmd_start(true);
    tx_ctrl.write(interface)?;
    interface.write_raw_ap_register(&sec_ap, regs::TX_DATA, words[0])?;

    if words.len() > 1 {
        // Wait for the first word to be consumed, then clear CMD_START before
        // sending continuation words.
        poll_tx_ctrl(interface, timeout)?;
        interface.write_raw_ap_register(&sec_ap, regs::TX_CTRL, 0)?;

        for &word in &words[1..] {
            poll_tx_ctrl(interface, timeout)?;
            interface.write_raw_ap_register(&sec_ap, regs::TX_DATA, word)?;
        }
    }

    // Final poll to confirm the last word has been consumed.
    poll_tx_ctrl(interface, timeout)?;

    Ok(())
}

/// Read one response word from the device.
pub fn read_response(interface: &mut dyn DapAccess, timeout: Duration) -> Result<u32, ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
    poll_rx_ctrl(interface, timeout)?;
    let response = interface.read_raw_ap_register(&sec_ap, regs::RX_DATA)?;
    Ok(response)
}

/// Decode a SACI response word and return an error if the result code is not Success.
pub fn check_result(response: u32, context: &str) -> Result<(), ArmError> {
    let result = SaciResult::from(((response >> 16) & 0xFF) as u8);

    if result != SaciResult::Success {
        tracing::error!(
            "SACI {} failed: {:?} (raw response: 0x{:08X})",
            context,
            result,
            response
        );

        return Err(ArmError::Other(format!(
            "SACI {} failed: {:?}",
            context, result
        )));
    }

    Ok(())
}

/// Read the Device Status Register from the CFG-AP.
pub fn read_device_status(interface: &mut dyn DapAccess) -> Result<DeviceStatusRegister, ArmError> {
    let cfg_ap: FullyQualifiedApAddress = ApSel::CfgAp.into();
    let val = interface.read_raw_ap_register(&cfg_ap, DEVICE_STATUS_ADDRESS)?;
    Ok(DeviceStatusRegister(val))
}

// ---- Utility functions ------------------------------------------------------

/// Build the first parameter word (header) for a SACI command.
///
/// Layout per ROM source: bits[7:0] = cmd_id, bits[15:8] = resp_seq_num (0),
/// bits[31:16] = cmd_specific.
pub fn make_header(cmd_id: u32, cmd_specific: u32) -> u32 {
    (cmd_specific << 16) | cmd_id
}

/// Pack a byte slice into little-endian u32 words, padding the final word with pad.
pub fn pack_words(data: &[u8], pad: u8) -> Vec<u32> {
    data.chunks(4)
        .map(|chunk| {
            let mut word = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                word |= (byte as u32) << (i * 8);
            }
            for i in chunk.len()..4 {
                word |= (pad as u32) << (i * 8);
            }
            word
        })
        .collect()
}

/// Calculate CRC32 using the ISO-HDLC (CRC-32) polynomial.
///
/// Parameters from TI documentation:
///   CRC32_INIT  = 0xFFFFFFFF
///   CRC32_POLY  = 0x04C11DB7
///   CRC32_RPOLY = 0xEDB88320 (reflected)
///   CRC32_FINAL = 0xFFFFFFFF (XOR output)
pub fn crc32_iso_hdlc(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;

    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }

    crc ^ 0xFFFF_FFFF
}
