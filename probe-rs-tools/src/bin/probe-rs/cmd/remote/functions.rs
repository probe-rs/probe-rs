use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::cmd::remote::LocalSession;

pub mod attach;
pub mod list_probes;
pub mod read_memory;
pub mod resume;
pub mod write_memory;

pub trait RemoteFunction: Serialize + Into<RemoteFunctions> {
    type Result: DeserializeOwned;

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result>;
}

/// The functions that can be called remotely.
#[derive(Serialize, Deserialize)]
pub enum RemoteFunctions {
    Attach(attach::Attach),
    ListProbes(list_probes::ListProbes),
    ReadMemory8(read_memory::ReadMemory8),
    ReadMemory16(read_memory::ReadMemory16),
    ReadMemory32(read_memory::ReadMemory32),
    ReadMemory64(read_memory::ReadMemory64),
    WriteMemory8(write_memory::WriteMemory8),
    WriteMemory16(write_memory::WriteMemory16),
    WriteMemory32(write_memory::WriteMemory32),
    WriteMemory64(write_memory::WriteMemory64),
    ResumeAllCores(resume::ResumeAllCores),
}

#[cfg(feature = "remote")]
impl RemoteFunctions {
    pub async fn run_on_server(self, iface: &mut LocalSession) -> anyhow::Result<String> {
        let result = match self {
            RemoteFunctions::Attach(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ListProbes(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ReadMemory8(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ReadMemory16(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ReadMemory32(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ReadMemory64(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::WriteMemory8(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::WriteMemory16(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::WriteMemory32(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::WriteMemory64(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ResumeAllCores(func) => serde_json::to_string(&func.run(iface).await?),
        };

        result.map_err(|e| e.into())
    }
}
