//! The parts of the MCD (Multi-Core Debug) API v1.1 that the MCD probe uses.
//!
//! The types and constants are transcribed from the SPRINT MCD API header `mcd_api.h`
//! (version 1.1). Only the functions for connecting, memory transactions, reset and error
//! information are declared. The original copyright notice and license are reproduced below,
//! as the license requires.
//!
//! Modification notice: this file is a partial Rust translation of `mcd_api.h` and
//! `mcd_types.h`, made by the probe-rs contributors on 2026-10-23.
//!
//! ```text
//! Copyright (c) 2008, ARM Ltd., Infineon Technologies, NXP Semiconductors,
//! Lauterbach, STMicroelectronics and TIMA Laboratory.
//! All rights reserved.
//!
//! Any redistribution and use of the SOFTWARE in source and binary forms,
//! with or without modification constitutes the full acceptance of the
//! following disclaimer as well as of the license herein and is permitted
//! provided that the following conditions are met:
//! - Redistributions of source code must retain the above copyright notice,
//!   this list of conditions and the disclaimer detailed below.
//! - Redistributions in binary form must reproduce the above copyright notice,
//!   this list of conditions and the disclaimer detailed below in the documentation
//!   and/or other materials provided with the distribution.
//! - Neither the name of its copyright holders nor the names of its contributors
//!   may be used to endorse or promote products derived from the Software
//!   without specific prior written permission.
//! - Modification of any or all of the source code, documentation and other
//!   materials provided under this license are subject to acknowledgement
//!   of the modification(s) by including a prominent notice on the modification(s)
//!   stating the change(s) to the file(s), identifying the date of such change
//!   and stating the name of the publisher of any such modification(s).
//!
//! THE SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
//! IMPLIED WARRANTIES OF MERCHANTABILITY, NON-INFRINGEMENT AND FITNESS FOR A
//! PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
//! OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
//! EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
//! PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS;
//! OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//! WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE,
//! MISREPRESENTATION OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//! SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//! ```

#![allow(non_camel_case_types, missing_docs)]

use std::ffi::{c_char, c_void};

/// `mcd_bool_t`: 8 bits on Linux and macOS, 32 bits elsewhere (API v1.1).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type mcd_bool_t = u8;
/// `mcd_bool_t`: 8 bits on Linux and macOS, 32 bits elsewhere (API v1.1).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub type mcd_bool_t = u32;

pub type mcd_return_et = u32;
pub type mcd_error_code_et = u32;

pub const MCD_API_VER_MAJOR: u16 = 1;
pub const MCD_API_VER_MINOR: u16 = 1;
pub const MCD_API_VER_AUTHOR: &[u8] = b"SPRINT Release";

pub const MCD_HOSTNAME_LEN: usize = 64;
pub const MCD_INFO_STR_LEN: usize = 256;
pub const MCD_KEY_LEN: usize = 64;
pub const MCD_UNIQUE_NAME_LEN: usize = 64;
pub const MCD_API_IMP_VENDOR_LEN: usize = 32;
/// The maximum number of transactions in one transaction list.
pub const MCD_TX_NUM_MAX: usize = 64;
/// The payload of a transaction list that every implementation supports.
pub const MCD_GUARANTEED_MIN_PAYLOAD: u32 = 16384;

pub const MCD_RET_ACT_NONE: mcd_return_et = 0;

pub const MCD_ERR_NONE: mcd_error_code_et = 0;
pub const MCD_ERR_TXLIST_READ: mcd_error_code_et = 0x2100;
pub const MCD_ERR_TXLIST_WRITE: mcd_error_code_et = 0x2101;
pub const MCD_ERR_TXLIST_TX: mcd_error_code_et = 0x2102;

pub const MCD_NOTUSED_ID: u32 = 0;

pub const MCD_TX_AT_R: u32 = 1;
pub const MCD_TX_AT_W: u32 = 2;
pub const MCD_TX_OPT_DEFAULT: u32 = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mcd_api_version_st {
    pub v_api_major: u16,
    pub v_api_minor: u16,
    pub author: [c_char; MCD_API_IMP_VENDOR_LEN],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mcd_impl_version_info_st {
    pub v_api: mcd_api_version_st,
    pub v_imp_major: u16,
    pub v_imp_minor: u16,
    pub v_imp_build: u16,
    pub vendor: [c_char; MCD_API_IMP_VENDOR_LEN],
    pub date: [c_char; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mcd_error_info_st {
    pub return_status: mcd_return_et,
    pub error_code: mcd_error_code_et,
    pub error_events: u32,
    pub error_str: [c_char; MCD_INFO_STR_LEN],
}

#[repr(C)]
#[derive(Debug)]
pub struct mcd_server_st {
    pub instance: *mut c_void,
    pub host: *const c_char,
    pub config_string: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mcd_core_con_info_st {
    pub host: [c_char; MCD_HOSTNAME_LEN],
    pub server_port: u32,
    pub server_key: [c_char; MCD_KEY_LEN],
    pub system_key: [c_char; MCD_KEY_LEN],
    pub device_key: [c_char; MCD_KEY_LEN],
    pub system: [c_char; MCD_UNIQUE_NAME_LEN],
    pub system_instance: [c_char; MCD_UNIQUE_NAME_LEN],
    pub acc_hw: [c_char; MCD_UNIQUE_NAME_LEN],
    pub device_type: u32,
    pub device: [c_char; MCD_UNIQUE_NAME_LEN],
    pub device_id: u32,
    pub core: [c_char; MCD_UNIQUE_NAME_LEN],
    pub core_type: u32,
    pub core_id: u32,
}

#[repr(C)]
#[derive(Debug)]
pub struct mcd_core_st {
    pub instance: *mut c_void,
    pub core_con_info: *const mcd_core_con_info_st,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mcd_addr_st {
    pub address: u64,
    pub mem_space_id: u32,
    pub addr_space_id: u32,
    pub addr_space_type: u32,
}

#[repr(C)]
#[derive(Debug)]
pub struct mcd_tx_st {
    pub addr: mcd_addr_st,
    pub access_type: u32,
    pub options: u32,
    pub access_width: u8,
    pub core_mode: u8,
    pub data: *mut u8,
    pub num_bytes: u32,
    pub num_bytes_ok: u32,
}

#[repr(C)]
#[derive(Debug)]
pub struct mcd_txlist_st {
    pub tx: *mut mcd_tx_st,
    pub num_tx: u32,
    pub num_tx_ok: u32,
}

pub type mcd_initialize_f = unsafe extern "C" fn(
    version_req: *const mcd_api_version_st,
    impl_info: *mut mcd_impl_version_info_st,
) -> mcd_return_et;
pub type mcd_exit_f = unsafe extern "C" fn();
pub type mcd_open_server_f = unsafe extern "C" fn(
    system_key: *const c_char,
    config_string: *const c_char,
    server: *mut *mut mcd_server_st,
) -> mcd_return_et;
pub type mcd_close_server_f = unsafe extern "C" fn(server: *const mcd_server_st) -> mcd_return_et;
pub type mcd_qry_systems_f = unsafe extern "C" fn(
    start_index: u32,
    num_systems: *mut u32,
    system_con_info: *mut mcd_core_con_info_st,
) -> mcd_return_et;
pub type mcd_qry_devices_f = unsafe extern "C" fn(
    system_con_info: *const mcd_core_con_info_st,
    start_index: u32,
    num_devices: *mut u32,
    device_con_info: *mut mcd_core_con_info_st,
) -> mcd_return_et;
pub type mcd_qry_cores_f = unsafe extern "C" fn(
    connection_info: *const mcd_core_con_info_st,
    start_index: u32,
    num_cores: *mut u32,
    core_con_info: *mut mcd_core_con_info_st,
) -> mcd_return_et;
pub type mcd_open_core_f = unsafe extern "C" fn(
    core_con_info: *const mcd_core_con_info_st,
    core: *mut *mut mcd_core_st,
) -> mcd_return_et;
pub type mcd_close_core_f = unsafe extern "C" fn(core: *const mcd_core_st) -> mcd_return_et;
pub type mcd_qry_error_info_f =
    unsafe extern "C" fn(core: *const mcd_core_st, error_info: *mut mcd_error_info_st);
pub type mcd_qry_max_payload_size_f =
    unsafe extern "C" fn(core: *const mcd_core_st, max_payload: *mut u32) -> mcd_return_et;
pub type mcd_execute_txlist_f =
    unsafe extern "C" fn(core: *const mcd_core_st, txlist: *mut mcd_txlist_st) -> mcd_return_et;
pub type mcd_qry_rst_classes_f =
    unsafe extern "C" fn(core: *const mcd_core_st, rst_class_vector: *mut u32) -> mcd_return_et;
pub type mcd_rst_f = unsafe extern "C" fn(
    core: *const mcd_core_st,
    rst_class_vector: u32,
    rst_and_halt: mcd_bool_t,
) -> mcd_return_et;

/// The MCD functions the probe calls.
#[derive(Debug, Clone, Copy)]
pub struct McdFunctions {
    pub initialize: mcd_initialize_f,
    pub exit: mcd_exit_f,
    pub open_server: mcd_open_server_f,
    pub close_server: mcd_close_server_f,
    pub qry_systems: mcd_qry_systems_f,
    pub qry_devices: mcd_qry_devices_f,
    pub qry_cores: mcd_qry_cores_f,
    pub open_core: mcd_open_core_f,
    pub close_core: mcd_close_core_f,
    pub qry_error_info: mcd_qry_error_info_f,
    pub qry_max_payload_size: mcd_qry_max_payload_size_f,
    pub execute_txlist: mcd_execute_txlist_f,
    pub qry_rst_classes: mcd_qry_rst_classes_f,
    pub rst: mcd_rst_f,
}

impl McdFunctions {
    /// Resolves the functions in `library`.
    ///
    /// # Safety
    ///
    /// `library` must be an MCD API v1.1 implementation, so that its exports have the signatures
    /// declared here.
    pub unsafe fn load(library: &libloading::Library) -> Result<Self, libloading::Error> {
        // SAFETY: the caller guarantees the signatures; the pointers are only used while the
        // library stays loaded, which the owner of `McdFunctions` ensures.
        unsafe {
            Ok(Self {
                initialize: *library.get(b"mcd_initialize_f\0")?,
                exit: *library.get(b"mcd_exit_f\0")?,
                open_server: *library.get(b"mcd_open_server_f\0")?,
                close_server: *library.get(b"mcd_close_server_f\0")?,
                qry_systems: *library.get(b"mcd_qry_systems_f\0")?,
                qry_devices: *library.get(b"mcd_qry_devices_f\0")?,
                qry_cores: *library.get(b"mcd_qry_cores_f\0")?,
                open_core: *library.get(b"mcd_open_core_f\0")?,
                close_core: *library.get(b"mcd_close_core_f\0")?,
                qry_error_info: *library.get(b"mcd_qry_error_info_f\0")?,
                qry_max_payload_size: *library.get(b"mcd_qry_max_payload_size_f\0")?,
                execute_txlist: *library.get(b"mcd_execute_txlist_f\0")?,
                qry_rst_classes: *library.get(b"mcd_qry_rst_classes_f\0")?,
                rst: *library.get(b"mcd_rst_f\0")?,
            })
        }
    }
}

/// A zeroed C structure.
pub fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for the plain-data `repr(C)` structures of this module, for which all
    // zero bytes are a valid value.
    unsafe { std::mem::zeroed() }
}

/// The string in a fixed-size C character array.
pub fn c_string(chars: &[c_char]) -> String {
    let bytes = chars
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Copies `text` into a fixed-size C character array, truncated and zero terminated.
pub fn set_c_string(chars: &mut [c_char], text: &[u8]) {
    let n = text.len().min(chars.len().saturating_sub(1));
    for (dst, &src) in chars.iter_mut().zip(&text[..n]) {
        *dst = src as c_char;
    }
    for dst in &mut chars[n..] {
        *dst = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The structure layout of the header, as compiled by MSVC for x64.
    #[test]
    #[cfg(all(target_os = "windows", target_pointer_width = "64"))]
    fn structures_have_the_header_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<mcd_api_version_st>(), 36);
        assert_eq!(size_of::<mcd_impl_version_info_st>(), 90);
        assert_eq!(size_of::<mcd_error_info_st>(), 268);
        assert_eq!(size_of::<mcd_core_con_info_st>(), 596);
        assert_eq!(offset_of!(mcd_core_con_info_st, device_type), 452);
        assert_eq!(size_of::<mcd_addr_st>(), 24);
        assert_eq!(size_of::<mcd_tx_st>(), 56);
        assert_eq!(offset_of!(mcd_tx_st, data), 40);
        assert_eq!(size_of::<mcd_txlist_st>(), 16);
        assert_eq!(size_of::<mcd_server_st>(), 24);
        assert_eq!(size_of::<mcd_core_st>(), 16);
    }

    #[test]
    fn c_strings_round_trip() {
        let mut chars = [0 as c_char; 8];
        set_c_string(&mut chars, b"Core 0: CPU0");
        assert_eq!(c_string(&chars), "Core 0:");
        set_c_string(&mut chars, b"ab");
        assert_eq!(c_string(&chars), "ab");
    }
}
