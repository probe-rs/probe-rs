use crate::definitions;
use jsonrpc_core::types::error::{Error, ErrorCode};
use jsonrpc_core::Result;
use probe_rs::{
    config::MemoryRegion, Architecture, CoreInformation, CoreRegisterAddress, CoreStatus, CoreType,
    DebugProbeInfo, MemoryInterface, Probe, Session,
};
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::Mutex;
use std::time::Duration;

pub struct Server {
    session: Mutex<RefCell<Option<Session>>>,
}

// The standard From trait can not be implemented for Error from probe_rs::Error
// as everything is defined outside of this crate. Thus we use this crate internal
// extension trait.
trait IntoJsonError {
    fn into_json_error(self) -> Error;
}

impl IntoJsonError for probe_rs::Error {
    fn into_json_error(self) -> Error {
        // TODO: Actually implement error handling
        Error {
            code: ErrorCode::ServerError(500),
            message: String::from("Some error occured"),
            data: None,
        }
    }
}

impl IntoJsonError for probe_rs::DebugProbeError {
    fn into_json_error(self) -> Error {
        // TODO: Actually implement error handling
        Error {
            code: ErrorCode::ServerError(502),
            message: String::from("Some probe error occured"),
            data: None,
        }
    }
}

impl Server {
    pub fn new() -> Self {
        Server {
            session: Mutex::new(RefCell::new(None)),
        }
    }

    pub fn new_with_session(session: Session) -> Self {
        Server {
            session: Mutex::new(RefCell::new(Some(session))),
        }
    }

    fn with_mut_session<F, R>(&self, f: F) -> Result<R>
    where
        F: Fn(&mut Session) -> Result<R>,
    {
        if let Some(ref mut session) = self.session.lock().unwrap().borrow_mut().deref_mut() {
            f(session)
        } else {
            Err(Error {
                code: ErrorCode::ServerError(501),
                message: String::from("A method that requires a session was accessed before a session had been opened"),
                data: None
            })
        }
    }

    fn with_session<F, R>(&self, f: F) -> Result<R>
    where
        F: Fn(&Session) -> Result<R>,
    {
        if let Some(session) = self.session.lock().unwrap().borrow().deref() {
            f(session)
        } else {
            Err(Error {
                code: ErrorCode::ServerError(501),
                message: String::from("A method that requires a session was accessed before a session had been opened"),
                data: None
            })
        }
    }
}

impl definitions::ProbeRsServer for Server {
    fn attach(&self, probe: DebugProbeInfo, chip: String) -> Result<()> {
        let probe = probe.open().map_err(|e| e.into_json_error())?;
        let session = probe.attach(chip).map_err(|e| e.into_json_error())?;
        self.session.lock().unwrap().replace(Some(session));
        Ok(())
    }
    fn list_probes(&self) -> Result<Vec<DebugProbeInfo>> {
        Ok(Probe::list_all())
    }
    fn has_session(&self) -> Result<bool> {
        Ok(self.session.lock().unwrap().borrow().deref().is_some())
    }
    fn list_cores(&self) -> Result<Vec<(usize, CoreType)>> {
        self.with_session(|s| Ok(s.list_cores()))
    }
    fn memory_map(&self) -> Result<Vec<MemoryRegion>> {
        self.with_session(|s| {
            let mut result = Vec::new();
            result.extend_from_slice(s.memory_map());
            Ok(result)
        })
    }
    fn architecture(&self) -> Result<Architecture> {
        self.with_session(|s| Ok(s.architecture()))
    }
    fn core_halted(&self, core_index: usize) -> Result<bool> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.core_halted().map_err(|e| e.into_json_error())
        })
    }
    fn halt(&self, core_index: usize, timeout: Duration) -> Result<CoreInformation> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.halt(timeout).map_err(|e| e.into_json_error())
        })
    }
    fn run(&self, core_index: usize) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.run().map_err(|e| e.into_json_error())
        })
    }
    fn reset(&self, core_index: usize) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.reset().map_err(|e| e.into_json_error())
        })
    }
    fn reset_and_halt(&self, core_index: usize, timeout: Duration) -> Result<CoreInformation> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.reset_and_halt(timeout)
                .map_err(|e| e.into_json_error())
        })
    }
    fn step(&self, core_index: usize) -> Result<CoreInformation> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.step().map_err(|e| e.into_json_error())
        })
    }
    fn status(&self, core_index: usize) -> Result<CoreStatus> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.status().map_err(|e| e.into_json_error())
        })
    }
    fn read_core_reg(&self, core_index: usize, address: u16) -> Result<u32> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.read_core_reg(address).map_err(|e| e.into_json_error())
        })
    }
    fn write_core_reg(
        &self,
        core_index: usize,
        address: CoreRegisterAddress,
        value: u32,
    ) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.write_core_reg(address, value)
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn get_available_breakpoint_units(&self, core_index: usize) -> Result<u32> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.get_available_breakpoint_units()
                .map_err(|e| e.into_json_error())
        })
    }
    fn set_hw_breakpoint(&self, core_index: usize, address: u32) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.set_hw_breakpoint(address)
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn clear_hw_breakpoint(&self, core_index: usize, address: u32) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.clear_hw_breakpoint(address)
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn core_architecture(&self, core_index: usize) -> Result<Architecture> {
        self.with_mut_session(|s| {
            let core = s.core(core_index).map_err(|e| e.into_json_error())?;
            Ok(core.architecture())
        })
    }
    fn read_word_32(&self, core_index: usize, address: u32) -> Result<u32> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.read_word_32(address).map_err(|e| e.into_json_error())
        })
    }
    fn read_word_8(&self, core_index: usize, address: u32) -> Result<u8> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.read_word_8(address).map_err(|e| e.into_json_error())
        })
    }
    fn read_32(&self, core_index: usize, address: u32, length: usize) -> Result<Vec<u32>> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            let mut vec = Vec::with_capacity(length);
            core.read_32(address, &mut vec[..])
                .map_err(|e| e.into_json_error())?;
            Ok(vec)
        })
    }
    fn read_8(&self, core_index: usize, address: u32, length: usize) -> Result<Vec<u8>> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            let mut vec = Vec::with_capacity(length);
            core.read_8(address, &mut vec[..])
                .map_err(|e| e.into_json_error())?;
            Ok(vec)
        })
    }
    fn write_word_32(&self, core_index: usize, addr: u32, data: u32) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.write_word_32(addr, data)
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn write_word_8(&self, core_index: usize, addr: u32, data: u8) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.write_word_8(addr, data)
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn write_32(&self, core_index: usize, addr: u32, data: Vec<u32>) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.write_32(addr, &data[..])
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
    fn write_8(&self, core_index: usize, addr: u32, data: Vec<u8>) -> Result<()> {
        self.with_mut_session(|s| {
            let mut core = s.core(core_index).map_err(|e| e.into_json_error())?;
            core.write_8(addr, &data[..])
                .map_err(|e| e.into_json_error())
        })?;
        Ok(())
    }
}
