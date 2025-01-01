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
    ReadMemory(read_memory::ReadMemory),
    WriteMemory(write_memory::WriteMemory),
    ResumeAllCores(resume::ResumeAllCores),
}

#[cfg(feature = "remote")]
impl RemoteFunctions {
    pub async fn run_on_server(self, iface: &mut LocalSession) -> anyhow::Result<String> {
        let result = match self {
            RemoteFunctions::Attach(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ListProbes(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ReadMemory(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::WriteMemory(func) => serde_json::to_string(&func.run(iface).await?),
            RemoteFunctions::ResumeAllCores(func) => serde_json::to_string(&func.run(iface).await?),
        };

        result.map_err(|e| e.into())
    }
}
