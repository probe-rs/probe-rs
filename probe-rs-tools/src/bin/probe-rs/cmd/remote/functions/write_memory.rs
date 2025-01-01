use crate::{
    cmd::remote::{functions::RemoteFunctions, LocalSession, SessionId},
    util::common_options::ReadWriteBitWidth,
};
use probe_rs::MemoryInterface as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct WriteMemory {
    pub sessid: SessionId,
    pub core: usize,
    pub address: u64,
    pub data: Vec<u64>,
    pub width: ReadWriteBitWidth,
}

impl super::RemoteFunction for WriteMemory {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> anyhow::Result<Self::Result> {
        let session = iface.session(self.sessid);
        let mut core = session.core(self.core).unwrap();

        match self.width {
            ReadWriteBitWidth::B8 => {
                let mut bvalues = Vec::new();
                for val in &self.data {
                    if *val > u8::MAX as u64 {
                        anyhow::bail!(
                            "{} in {:?} is too large for an 8 bit write.",
                            val,
                            self.data,
                        );
                    }
                    bvalues.push(*val as u8);
                }
                core.write_8(self.address, &bvalues)?;
            }
            ReadWriteBitWidth::B32 => {
                let mut bvalues = Vec::new();
                for val in &self.data {
                    if *val > u32::MAX as u64 {
                        anyhow::bail!(
                            "{} in {:?} is too large for a 32 bit write.",
                            val,
                            self.data,
                        );
                    }
                    bvalues.push(*val as u32);
                }
                core.write_32(self.address, &bvalues)?;
            }
            ReadWriteBitWidth::B64 => core.write_64(self.address, &self.data)?,
        }

        Ok(())
    }
}

impl From<WriteMemory> for RemoteFunctions {
    fn from(func: WriteMemory) -> Self {
        RemoteFunctions::WriteMemory(func)
    }
}
