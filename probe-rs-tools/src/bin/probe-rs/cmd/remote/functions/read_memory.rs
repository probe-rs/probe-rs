use crate::{
    cmd::remote::{functions::RemoteFunctions, LocalSession, SessionId},
    util::common_options::ReadWriteBitWidth,
};
use probe_rs::MemoryInterface as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ReadMemory {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub count: u64,
    pub width: ReadWriteBitWidth,
}

impl super::RemoteFunction for ReadMemory {
    type Result = Result<Vec<u64>, String>;

    async fn run(self, iface: &mut LocalSession) -> Self::Result {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();

        match self.width {
            ReadWriteBitWidth::B8 => {
                let mut words = vec![0; self.count as usize];
                core.read_8(self.address, &mut words).unwrap();
                Ok(words.into_iter().map(|i| i as u64).collect::<Vec<u64>>())
            }
            ReadWriteBitWidth::B32 => {
                let mut words = vec![0; self.count as usize];
                core.read_32(self.address, &mut words).unwrap();
                Ok(words.into_iter().map(|i| i as u64).collect::<Vec<u64>>())
            }
            ReadWriteBitWidth::B64 => {
                let mut words = vec![0; self.count as usize];
                core.read_64(self.address, &mut words).unwrap();
                Ok(words.into_iter().map(|i| i as u64).collect::<Vec<u64>>())
            }
        }
    }
}

impl From<ReadMemory> for RemoteFunctions {
    fn from(func: ReadMemory) -> Self {
        RemoteFunctions::ReadMemory(func)
    }
}
