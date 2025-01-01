use crate::cmd::remote::{functions::RemoteFunctions, LocalSession, SessionId};
use probe_rs::MemoryInterface as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct WriteMemory8 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct WriteMemory16 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub data: Vec<u16>,
}

#[derive(Serialize, Deserialize)]
pub struct WriteMemory32 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub data: Vec<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct WriteMemory64 {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub data: Vec<u64>,
}

impl super::RemoteFunction for WriteMemory8 {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();
        core.write_8(self.address, &self.data)?;
        Ok(())
    }
}

impl super::RemoteFunction for WriteMemory16 {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();
        core.write_16(self.address, &self.data)?;
        Ok(())
    }
}

impl super::RemoteFunction for WriteMemory32 {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();
        core.write_32(self.address, &self.data)?;
        Ok(())
    }
}

impl super::RemoteFunction for WriteMemory64 {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();
        core.write_64(self.address, &self.data)?;
        Ok(())
    }
}

impl From<WriteMemory8> for RemoteFunctions {
    fn from(func: WriteMemory8) -> Self {
        RemoteFunctions::WriteMemory8(func)
    }
}

impl From<WriteMemory16> for RemoteFunctions {
    fn from(func: WriteMemory16) -> Self {
        RemoteFunctions::WriteMemory16(func)
    }
}

impl From<WriteMemory32> for RemoteFunctions {
    fn from(func: WriteMemory32) -> Self {
        RemoteFunctions::WriteMemory32(func)
    }
}

impl From<WriteMemory64> for RemoteFunctions {
    fn from(func: WriteMemory64) -> Self {
        RemoteFunctions::WriteMemory64(func)
    }
}
