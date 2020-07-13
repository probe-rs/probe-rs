use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use probe_rs::{
    config::MemoryRegion, Architecture, CoreInformation, CoreRegisterAddress, CoreStatus, CoreType,
    DebugProbeInfo,
};
use std::time::Duration;

pub use self::gen_client::Client as ProbeRsGenClient;
pub use self::rpc_impl_ProbeRsServer::gen_server::ProbeRsServer as ProbeRsGenServer;

// TODO: This trait is actually a merge of Core and Session, if possible they should
// be split apart into two seperate #[rpc] traits in the future, I do not understand
// how to do that properly with jsonrpc_derive though.
#[rpc]
pub trait ProbeRsServer {
    #[rpc(name = "attach")]
    fn attach(&self, probe: DebugProbeInfo, chip: String) -> Result<()>;
    #[rpc(name = "listProbes")]
    fn list_probes(&self) -> Result<Vec<DebugProbeInfo>>;
    #[rpc(name = "hasSession")]
    fn has_session(&self) -> Result<bool>;
    #[rpc(name = "listCores")]
    fn list_cores(&self) -> Result<Vec<(usize, CoreType)>>;
    #[rpc(name = "memoryMap")]
    fn memory_map(&self) -> Result<Vec<MemoryRegion>>;
    #[rpc(name = "architecture")]
    fn architecture(&self) -> Result<Architecture>;
    #[rpc(name = "coreHalted")]
    fn core_halted(&self, core_index: usize) -> Result<bool>;
    #[rpc(name = "halt")]
    fn halt(&self, core_index: usize, timeout: Duration) -> Result<CoreInformation>;
    #[rpc(name = "run")]
    fn run(&self, core_index: usize) -> Result<()>;
    #[rpc(name = "reset")]
    fn reset(&self, core_index: usize) -> Result<()>;
    #[rpc(name = "resetAndHalt")]
    fn reset_and_halt(&self, core_index: usize, timeout: Duration) -> Result<CoreInformation>;
    #[rpc(name = "step")]
    fn step(&self, core_index: usize) -> Result<CoreInformation>;
    #[rpc(name = "status")]
    fn status(&self, core_index: usize) -> Result<CoreStatus>;
    #[rpc(name = "readCoreReg")]
    fn read_core_reg(&self, core_index: usize, address: u16) -> Result<u32>;
    #[rpc(name = "writeCoreReg")]
    fn write_core_reg(
        &self,
        core_index: usize,
        address: CoreRegisterAddress,
        value: u32,
    ) -> Result<()>;
    #[rpc(name = "getAvailableBreakpointUnits")]
    fn get_available_breakpoint_units(&self, core_index: usize) -> Result<u32>;
    #[rpc(name = "setHwBreakpoint")]
    fn set_hw_breakpoint(&self, core_index: usize, address: u32) -> Result<()>;
    #[rpc(name = "clearHwBreakpoint")]
    fn clear_hw_breakpoint(&self, core_index: usize, address: u32) -> Result<()>;
    #[rpc(name = "coreArchitecture")]
    fn core_architecture(&self, core_index: usize) -> Result<Architecture>;
    #[rpc(name = "readWord32")]
    fn read_word_32(&self, core_index: usize, address: u32) -> Result<u32>;
    #[rpc(name = "readWord8")]
    fn read_word_8(&self, core_index: usize, address: u32) -> Result<u8>;
    #[rpc(name = "read32")]
    fn read_32(&self, core_index: usize, address: u32, length: usize) -> Result<Vec<u32>>;
    #[rpc(name = "read8")]
    fn read_8(&self, core_index: usize, address: u32, length: usize) -> Result<Vec<u8>>;
    #[rpc(name = "writeWord32")]
    fn write_word_32(&self, core_index: usize, addr: u32, data: u32) -> Result<()>;
    #[rpc(name = "writeWord8")]
    fn write_word_8(&self, core_index: usize, addr: u32, data: u8) -> Result<()>;
    #[rpc(name = "write32")]
    fn write_32(&self, core_index: usize, addr: u32, data: Vec<u32>) -> Result<()>;
    #[rpc(name = "write8")]
    fn write_8(&self, core_index: usize, addr: u32, data: Vec<u8>) -> Result<()>;
}
