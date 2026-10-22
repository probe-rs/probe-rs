//! A debug probe that reaches the device through an MCD (Multi-Core Debug) API library.
//!
//! Debug tools for AURIX often ship an MCD API v1.1 implementation as a shared library (for
//! example `mcdxdas.dll` of the Infineon DAS installation, or `mcd_tas.dll`, an MCD library
//! over a TAS server). This probe loads such a library at run time and uses it for memory
//! transactions and reset only. Everything else (run control, breakpoints, stepping) is done by
//! the TriCore core implementation of probe-rs through the OCDS registers, exactly as with the
//! TAS probe.
//!
//! The library is not part of probe-rs; it is loaded from the path in the selector, or from the
//! environment variable `PROBE_RS_MCD_DLL` when the selector has no path:
//!
//! ```text
//! --probe "058b:0001:C:\tools\mcd_tas.dll"
//! --probe "058b:0001:C:\tools\mcd_tas.dll|McdHostName=192.168.1.10 McdServerPort=24817"
//! --probe 058b:0001                     with PROBE_RS_MCD_DLL (and PROBE_RS_MCD_CONFIG) set
//! ```
//!
//! The text after `|` is the configuration string passed to `mcd_open_server_f`. The probe
//! connects to the first system, the first device and its first core of the server.

pub mod ffi;

use std::{
    ffi::CString,
    fmt,
    path::{Path, PathBuf},
    ptr,
};

use probe_rs::{
    architecture::tricore::bus::{
        AddressMap, BusLimits, BusTransfer, ResetKind, TransferKind, TransferStatus,
        TriCoreBusAccess, TriCoreError,
    },
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
        ProbeFactory, WireProtocol, list::ProbeListItem,
    },
};

use ffi::McdFunctions;

use crate::tas::TAS_VID;

/// The placeholder product ID of an MCD probe, used with the Infineon vendor ID.
pub const MCD_PID: u16 = 0x0001;
/// The environment variable with the path of the MCD library.
pub const LIBRARY_ENV: &str = "PROBE_RS_MCD_DLL";
/// The environment variable with the MCD server configuration string.
pub const CONFIG_ENV: &str = "PROBE_RS_MCD_CONFIG";

/// The transfer status code of a read that the MCD library reported as failed.
pub const STATUS_READ_FAILED: u8 = 0x01;
/// The transfer status code of a write that the MCD library reported as failed.
pub const STATUS_WRITE_FAILED: u8 = 0x02;
/// The transfer status code of a transaction the MCD library refused for another reason.
pub const STATUS_TX_FAILED: u8 = 0x03;
/// The transfer status code of an address map the MCD library has no memory space for.
///
/// It is the TAS code for an access to a blocked address map.
pub const STATUS_MAP_NOT_AVAILABLE: u8 = 0x8D;

/// The block size of the transfers the probe asks for.
const MAX_BLOCK: usize = 1024;
/// The MCD memory space of the device address space (TAS map 0).
const MEM_SPACE_TARGET: u32 = 0;
/// The reset class of the system reset, in the class numbering of the Infineon MCD libraries.
const RESET_CLASS_SYSTEM: u32 = 1 << 1;

/// Where an MCD probe loads its library from and how it opens the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McdEndpoint {
    /// The path of the MCD library.
    pub library: PathBuf,
    /// The configuration string for `mcd_open_server_f`. Empty means none.
    pub config: String,
}

impl McdEndpoint {
    /// Parses the serial number part of a selector: `[<library>][|<config>]`. A missing part is
    /// taken from `env` (the environment variables [`LIBRARY_ENV`] and [`CONFIG_ENV`]).
    pub fn parse(serial: &str, env: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let (library, config) = match serial.split_once('|') {
            Some((library, config)) => (library.trim(), Some(config.trim())),
            None => (serial.trim(), None),
        };
        let library = match library {
            "" => env(LIBRARY_ENV).ok_or_else(|| {
                format!(
                    "no MCD library given: add its path to the probe selector or set {LIBRARY_ENV}"
                )
            })?,
            path => path.to_string(),
        };
        let config = match config {
            Some(config) => config.to_string(),
            None => env(CONFIG_ENV).unwrap_or_default(),
        };
        Ok(Self {
            library: PathBuf::from(library),
            config,
        })
    }

    /// The serial number form of this endpoint, as accepted by [`McdEndpoint::parse`].
    pub fn serial(&self) -> String {
        if self.config.is_empty() {
            self.library.display().to_string()
        } else {
            format!("{}|{}", self.library.display(), self.config)
        }
    }
}

fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Errors of the MCD probe.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum McdError {
    /// The MCD library {0} could not be loaded: {1}
    Load(PathBuf, libloading::Error),
    /// {function} failed with MCD error {code:#06x}: {message}
    Call {
        /// The MCD function.
        function: &'static str,
        /// The MCD error code.
        code: u32,
        /// The error text of the library.
        message: String,
    },
    /// The MCD server has no {0}.
    NothingToOpen(&'static str),
    /// The probe is not connected to a core.
    NotConnected,
}

impl probe_rs::probe::ProbeError for McdError {}

fn probe_error(error: McdError) -> DebugProbeError {
    DebugProbeError::ProbeSpecific(error.into())
}

fn bus_error(error: McdError) -> TriCoreError {
    TriCoreError::DebugProbe(probe_error(error))
}

/// The factory of [`McdProbe`]s.
#[derive(Debug)]
pub struct McdFactory;

impl fmt::Display for McdFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCD library")
    }
}

impl ProbeFactory for McdFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        if selector.vendor_id != TAS_VID || selector.product_id != MCD_PID {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }
        let endpoint =
            McdEndpoint::parse(selector.serial_number.as_deref().unwrap_or(""), environment)
                .map_err(DebugProbeError::Other)?;
        let probe = McdProbe::load(endpoint).map_err(|e| DebugProbeError::Other(e.to_string()))?;
        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        // Loading a library to look for servers has side effects, so only the configured
        // library is listed, without loading it.
        McdEndpoint::parse("", environment)
            .ok()
            .and_then(list_item)
            .into_iter()
            .collect()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        match selector {
            None => self.list_probes(),
            // A selector with a library path names a probe that is not listed otherwise.
            Some(selector) if selector.vendor_id == TAS_VID && selector.product_id == MCD_PID => {
                McdEndpoint::parse(selector.serial_number.as_deref().unwrap_or(""), environment)
                    .ok()
                    .and_then(list_item)
                    .into_iter()
                    .collect()
            }
            Some(_) => Vec::new(),
        }
    }
}

/// The list entry of `endpoint`, if its library exists.
fn list_item(endpoint: McdEndpoint) -> Option<ProbeListItem> {
    if !endpoint.library.is_file() {
        return None;
    }
    let name = endpoint
        .library
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(ProbeListItem::accessible(DebugProbeInfo::new(
        format!("MCD {name}"),
        TAS_VID,
        MCD_PID,
        Some(endpoint.serial()),
        &McdFactory,
        None,
        false,
    )))
}

/// A connection to one core of a device through an MCD library.
pub struct McdProbe {
    endpoint: McdEndpoint,
    api: McdFunctions,
    initialized: bool,
    server: *mut ffi::mcd_server_st,
    core: *mut ffi::mcd_core_st,
    core_info: ffi::mcd_core_con_info_st,
    max_payload: u32,
    reset_class: u32,
    connect_under_reset: bool,
    // Dropped last, after `Drop::drop` has closed the connections.
    _library: Option<libloading::Library>,
}

// SAFETY: the raw pointers are handles of the MCD library. They are only used through `&mut
// self`, so from one thread at a time, which MCD libraries support.
unsafe impl Send for McdProbe {}

impl fmt::Debug for McdProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McdProbe")
            .field("endpoint", &self.endpoint)
            .field("connected", &!self.core.is_null())
            .field("core", &ffi::c_string(&self.core_info.core))
            .finish()
    }
}

impl McdProbe {
    /// Loads the MCD library of `endpoint`. It connects on [`DebugProbe::attach`].
    pub fn load(endpoint: McdEndpoint) -> Result<Self, McdError> {
        let error = |e| McdError::Load(endpoint.library.clone(), e);
        // SAFETY: loading a library runs its initialisation code. The user selected the library
        // as an MCD implementation.
        let library = unsafe { libloading::Library::new(&endpoint.library) }.map_err(error)?;
        // SAFETY: an MCD API v1.1 library exports these functions with these signatures.
        let api = unsafe { McdFunctions::load(&library) }.map_err(error)?;
        let mut probe = Self::with_functions(endpoint, api);
        probe._library = Some(library);
        Ok(probe)
    }

    /// Creates a probe that calls `api`, for example functions linked into the program.
    pub fn with_functions(endpoint: McdEndpoint, api: McdFunctions) -> Self {
        Self {
            endpoint,
            api,
            initialized: false,
            server: ptr::null_mut(),
            core: ptr::null_mut(),
            core_info: ffi::zeroed(),
            max_payload: ffi::MCD_GUARANTEED_MIN_PAYLOAD,
            reset_class: RESET_CLASS_SYSTEM,
            connect_under_reset: false,
            _library: None,
        }
    }

    /// The path of the MCD library.
    pub fn library(&self) -> &Path {
        &self.endpoint.library
    }

    /// The name of the connected core, as the MCD library reports it.
    pub fn core_name(&self) -> String {
        ffi::c_string(&self.core_info.core)
    }

    /// The name of the access hardware of the connected device.
    pub fn access_hardware(&self) -> String {
        ffi::c_string(&self.core_info.acc_hw)
    }

    /// Turns an MCD return value into a result, with the library's error information.
    fn check(&self, function: &'static str, ret: ffi::mcd_return_et) -> Result<(), McdError> {
        if ret == ffi::MCD_RET_ACT_NONE {
            return Ok(());
        }
        let (code, message) = self.error_info();
        Err(McdError::Call {
            function,
            code,
            message,
        })
    }

    fn error_info(&self) -> (u32, String) {
        let mut info: ffi::mcd_error_info_st = ffi::zeroed();
        // SAFETY: `info` is a valid output structure; a null core asks for the last error.
        unsafe { (self.api.qry_error_info)(self.core, &mut info) };
        (info.error_code, ffi::c_string(&info.error_str))
    }

    fn connect(&mut self) -> Result<(), McdError> {
        let mut version: ffi::mcd_api_version_st = ffi::zeroed();
        version.v_api_major = ffi::MCD_API_VER_MAJOR;
        version.v_api_minor = ffi::MCD_API_VER_MINOR;
        ffi::set_c_string(&mut version.author, ffi::MCD_API_VER_AUTHOR);
        let mut implementation: ffi::mcd_impl_version_info_st = ffi::zeroed();
        // SAFETY: valid input and output structures.
        let ret = unsafe { (self.api.initialize)(&version, &mut implementation) };
        self.check("mcd_initialize_f", ret)?;
        self.initialized = true;
        tracing::info!(
            "MCD library {} {}.{}.{} (API {}.{}), {}",
            ffi::c_string(&implementation.vendor),
            implementation.v_imp_major,
            implementation.v_imp_minor,
            implementation.v_imp_build,
            implementation.v_api.v_api_major,
            implementation.v_api.v_api_minor,
            self.endpoint.library.display()
        );

        let config = CString::new(self.endpoint.config.as_str()).map_err(|_| McdError::Call {
            function: "mcd_open_server_f",
            code: 0,
            message: "the configuration string contains a NUL character".into(),
        })?;
        let config_ptr = if self.endpoint.config.is_empty() {
            ptr::null()
        } else {
            config.as_ptr()
        };
        let mut server = ptr::null_mut();
        // SAFETY: `config` outlives the call; `server` receives the server handle.
        let ret = unsafe { (self.api.open_server)(ptr::null(), config_ptr, &mut server) };
        self.check("mcd_open_server_f", ret)?;
        if server.is_null() {
            return Err(McdError::NothingToOpen("server"));
        }
        self.server = server;

        let mut system: ffi::mcd_core_con_info_st = ffi::zeroed();
        let mut count = 1;
        // SAFETY: room for `count` entries.
        let ret = unsafe { (self.api.qry_systems)(0, &mut count, &mut system) };
        self.check("mcd_qry_systems_f", ret)?;
        if count == 0 {
            return Err(McdError::NothingToOpen("system"));
        }

        let mut device: ffi::mcd_core_con_info_st = ffi::zeroed();
        let mut count = 1;
        // SAFETY: room for `count` entries.
        let ret = unsafe { (self.api.qry_devices)(&system, 0, &mut count, &mut device) };
        self.check("mcd_qry_devices_f", ret)?;
        if count == 0 {
            return Err(McdError::NothingToOpen("device"));
        }

        let mut core_info: ffi::mcd_core_con_info_st = ffi::zeroed();
        let mut count = 1;
        // SAFETY: room for `count` entries.
        let ret = unsafe { (self.api.qry_cores)(&device, 0, &mut count, &mut core_info) };
        self.check("mcd_qry_cores_f", ret)?;
        if count == 0 {
            return Err(McdError::NothingToOpen("core"));
        }

        let mut core = ptr::null_mut();
        // SAFETY: `core_info` is the library's own description of the core.
        let ret = unsafe { (self.api.open_core)(&core_info, &mut core) };
        self.check("mcd_open_core_f", ret)?;
        if core.is_null() {
            return Err(McdError::NothingToOpen("core"));
        }
        self.core = core;
        // SAFETY: the library owns the core object while the core is open.
        let opened_info = unsafe { (*core).core_con_info };
        if !opened_info.is_null() {
            // SAFETY: as above.
            core_info = unsafe { *opened_info };
        }
        self.core_info = core_info;

        let mut max_payload = 0;
        // SAFETY: valid output.
        let ret = unsafe { (self.api.qry_max_payload_size)(self.core, &mut max_payload) };
        if ret == ffi::MCD_RET_ACT_NONE && max_payload > 0 {
            self.max_payload = max_payload;
        }

        let mut classes = 0;
        // SAFETY: valid output.
        let ret = unsafe { (self.api.qry_rst_classes)(self.core, &mut classes) };
        if ret == ffi::MCD_RET_ACT_NONE && classes != 0 {
            self.reset_class = if classes & RESET_CLASS_SYSTEM != 0 {
                RESET_CLASS_SYSTEM
            } else {
                1 << classes.trailing_zeros()
            };
        }

        tracing::info!(
            "MCD: opened '{}' of {} on {} (JTAG ID {:#010x}, payload {} B, reset class {:#x})",
            ffi::c_string(&self.core_info.core),
            ffi::c_string(&self.core_info.device),
            ffi::c_string(&self.core_info.acc_hw),
            self.core_info.device_type,
            self.max_payload,
            self.reset_class
        );

        if self.connect_under_reset {
            self.reset_and_halt()?;
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.core.is_null() {
            // SAFETY: an open core handle.
            let ret = unsafe { (self.api.close_core)(self.core) };
            if let Err(error) = self.check("mcd_close_core_f", ret) {
                tracing::warn!("{error}");
            }
            self.core = ptr::null_mut();
        }
        if !self.server.is_null() {
            // SAFETY: an open server handle.
            let ret = unsafe { (self.api.close_server)(self.server) };
            if let Err(error) = self.check("mcd_close_server_f", ret) {
                tracing::warn!("{error}");
            }
            self.server = ptr::null_mut();
        }
        if self.initialized {
            // SAFETY: the library was initialised.
            unsafe { (self.api.exit)() };
            self.initialized = false;
        }
    }

    fn reset_and_halt(&mut self) -> Result<(), McdError> {
        if self.core.is_null() {
            return Err(McdError::NotConnected);
        }
        // SAFETY: an open core handle.
        let ret = unsafe { (self.api.rst)(self.core, self.reset_class, 1) };
        self.check("mcd_rst_f", ret)
    }

    /// Runs `transfers` as one transaction list. They must fit into one list.
    fn execute_list(&mut self, transfers: &mut [BusTransfer]) -> Result<(), McdError> {
        let mut list = Vec::with_capacity(transfers.len());
        for transfer in transfers.iter_mut() {
            let len = transfer.len();
            let (access_type, width) = match transfer.kind {
                TransferKind::Read { len } => {
                    transfer.data.clear();
                    transfer.data.resize(len, 0);
                    (ffi::MCD_TX_AT_R, len)
                }
                TransferKind::Write => (ffi::MCD_TX_AT_W, len),
            };
            list.push(ffi::mcd_tx_st {
                addr: ffi::mcd_addr_st {
                    address: u64::from(transfer.address),
                    mem_space_id: MEM_SPACE_TARGET,
                    addr_space_id: 0,
                    addr_space_type: ffi::MCD_NOTUSED_ID,
                },
                access_type,
                options: ffi::MCD_TX_OPT_DEFAULT,
                // Single accesses keep their width (registers need it); blocks leave it to the
                // library.
                access_width: if matches!(width, 1 | 2 | 4 | 8) {
                    width as u8
                } else {
                    0
                },
                core_mode: 0,
                data: transfer.data.as_mut_ptr(),
                num_bytes: len as u32,
                num_bytes_ok: 0,
            });
        }
        let mut txlist = ffi::mcd_txlist_st {
            tx: list.as_mut_ptr(),
            num_tx: list.len() as u32,
            num_tx_ok: 0,
        };
        // SAFETY: every transaction points into a buffer of `num_bytes` bytes that lives until
        // the end of this function.
        let ret = unsafe { (self.api.execute_txlist)(self.core, &mut txlist) };

        let done = (txlist.num_tx_ok as usize).min(transfers.len());
        for transfer in &mut transfers[..done] {
            transfer.status = Some(TransferStatus::Ok);
        }
        if ret == ffi::MCD_RET_ACT_NONE {
            if let Some(transfer) = transfers.get_mut(done) {
                // The library claims success but did not run every transaction.
                transfer.status = Some(TransferStatus::Failed(STATUS_TX_FAILED));
            }
            return Ok(());
        }
        let (code, message) = self.error_info();
        let status = match code {
            ffi::MCD_ERR_TXLIST_READ => STATUS_READ_FAILED,
            ffi::MCD_ERR_TXLIST_WRITE => STATUS_WRITE_FAILED,
            ffi::MCD_ERR_TXLIST_TX => STATUS_TX_FAILED,
            _ => {
                return Err(McdError::Call {
                    function: "mcd_execute_txlist_f",
                    code,
                    message,
                });
            }
        };
        match transfers.get_mut(done) {
            Some(transfer) => {
                tracing::debug!(
                    "MCD: transaction at {:#010x} failed: {message}",
                    transfer.address
                );
                transfer.status = Some(TransferStatus::Failed(status));
                Ok(())
            }
            None => Err(McdError::Call {
                function: "mcd_execute_txlist_f",
                code,
                message,
            }),
        }
    }
}

impl Drop for McdProbe {
    fn drop(&mut self) {
        self.close();
    }
}

impl DebugProbe for McdProbe {
    fn get_name(&self) -> &str {
        "MCD library"
    }

    fn speed_khz(&self) -> u32 {
        // The MCD library configures the interface.
        0
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        tracing::debug!("MCD: the library sets the interface speed, ignoring {speed_khz} kHz");
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.connect().map_err(|error| {
            self.close();
            probe_error(error)
        })
    }

    fn detach(&mut self) -> Result<(), probe_rs::Error> {
        self.close();
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.reset_and_halt().map_err(probe_error)
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        // The next attach resets the device after opening the core.
        self.connect_under_reset = true;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn set_reset_line_connected(&mut self, _connected: bool) {
        // The library drives the reset.
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        tracing::debug!("MCD: the library selects the wire protocol, ignoring {protocol}");
        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        None
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_tricore_bus(&mut self) -> Option<&mut dyn TriCoreBusAccess> {
        if self.core.is_null() {
            None
        } else {
            Some(self)
        }
    }
}

impl TriCoreBusAccess for McdProbe {
    fn execute(&mut self, transfers: &mut [BusTransfer]) -> Result<(), TriCoreError> {
        if self.core.is_null() {
            return Err(bus_error(McdError::NotConnected));
        }
        let max_payload = self.max_payload as usize;
        let mut start = 0;
        while start < transfers.len() {
            // The memory space of the device is the only one an MCD library offers.
            if transfers[start].map != AddressMap::Target {
                transfers[start].status = Some(TransferStatus::Failed(STATUS_MAP_NOT_AVAILABLE));
                return Ok(());
            }
            // One list: at most MCD_TX_NUM_MAX transactions within the payload limit, and only
            // transfers in the device memory space.
            let mut end = start;
            let mut payload = 0;
            while end < transfers.len()
                && end - start < ffi::MCD_TX_NUM_MAX
                && transfers[end].map == AddressMap::Target
                && (end == start || payload + transfers[end].len() <= max_payload)
            {
                payload += transfers[end].len();
                end += 1;
            }
            let batch = &mut transfers[start..end];
            self.execute_list(batch).map_err(bus_error)?;
            if batch.iter().any(|t| t.status != Some(TransferStatus::Ok)) {
                return Ok(());
            }
            start = end;
        }
        Ok(())
    }

    fn limits(&self) -> BusLimits {
        BusLimits {
            max_transfers: ffi::MCD_TX_NUM_MAX,
            max_block_size: MAX_BLOCK.min(self.max_payload as usize),
        }
    }

    fn reset(&mut self, kind: ResetKind) -> Result<(), TriCoreError> {
        match kind {
            ResetKind::ResetAndHalt => self.reset_and_halt().map_err(bus_error),
        }
    }

    fn jtag_id(&mut self) -> Result<u32, TriCoreError> {
        if self.core.is_null() {
            return Err(bus_error(McdError::NotConnected));
        }
        Ok(self.core_info.device_type)
    }
}

#[cfg(test)]
mod tests;
