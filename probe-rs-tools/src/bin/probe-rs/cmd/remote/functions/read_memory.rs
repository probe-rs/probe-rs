use crate::cmd::remote::{functions::RemoteFunctions, LocalSession, SessionId};
use probe_rs::MemoryInterface as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ReadMemory8 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub count: u64,
}

#[derive(Serialize, Deserialize)]
pub struct ReadMemory16 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub count: u64,
}

#[derive(Serialize, Deserialize)]
pub struct ReadMemory32 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub count: u64,
}

#[derive(Serialize, Deserialize)]
pub struct ReadMemory64 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub count: u64,
}

impl super::RemoteFunction for ReadMemory8 {
    type Result = Vec<u8>;

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core)?;

        let mut words = vec![0; self.count as usize];
        core.read_8(self.address, &mut words)?;
        Ok(words)
    }
}

impl super::RemoteFunction for ReadMemory16 {
    type Result = Vec<u16>;

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core)?;

        let mut words = vec![0; self.count as usize];
        core.read_16(self.address, &mut words)?;
        Ok(words)
    }
}

impl super::RemoteFunction for ReadMemory32 {
    type Result = Vec<u32>;

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core)?;

        let mut words = vec![0; self.count as usize];
        core.read_32(self.address, &mut words)?;
        Ok(words)
    }
}

impl super::RemoteFunction for ReadMemory64 {
    type Result = Vec<u64>;

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core)?;

        let mut words = vec![0; self.count as usize];
        core.read_64(self.address, &mut words)?;
        Ok(words)
    }
}

impl From<ReadMemory8> for RemoteFunctions {
    fn from(func: ReadMemory8) -> Self {
        RemoteFunctions::ReadMemory8(func)
    }
}

impl From<ReadMemory16> for RemoteFunctions {
    fn from(func: ReadMemory16) -> Self {
        RemoteFunctions::ReadMemory16(func)
    }
}

impl From<ReadMemory32> for RemoteFunctions {
    fn from(func: ReadMemory32) -> Self {
        RemoteFunctions::ReadMemory32(func)
    }
}

impl From<ReadMemory64> for RemoteFunctions {
    fn from(func: ReadMemory64) -> Self {
        RemoteFunctions::ReadMemory64(func)
    }
}
