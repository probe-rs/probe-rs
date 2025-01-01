use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub mod list_probes;

pub trait RemoteFunction: Serialize + Into<RemoteFunctions> {
    type Result: DeserializeOwned;

    async fn run(self) -> Self::Result;
}

/// The functions that can be called remotely.
#[derive(Serialize, Deserialize)]
pub enum RemoteFunctions {
    ListProbes(list_probes::ListProbes),
}

#[cfg(feature = "remote")]
impl RemoteFunctions {
    pub async fn run_on_server(self) -> anyhow::Result<String> {
        let result = match self {
            RemoteFunctions::ListProbes(func) => serde_json::to_string(&func.run().await),
        };

        result.map_err(|e| e.into())
    }
}
