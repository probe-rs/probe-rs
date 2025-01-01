use crate::cmd::remote::functions::read_memory::{
    ReadMemory16, ReadMemory32, ReadMemory64, ReadMemory8,
};
use crate::cmd::remote::functions::resume::ResumeAllCores;
use crate::cmd::remote::SessionInterface;
use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::CoreOptions;
use serde::{Deserialize, Serialize};

/// Read from target memory address
///
/// e.g. probe-rs read b32 0x400E1490 2
///      Reads 2 32-bit words from address 0x400E1490
///
/// Output is a space separated list of hex values padded to the read word width.
/// e.g. 2 words
///     00 00 (8-bit)
///     00000000 00000000 (32-bit)
///     0000000000000000 0000000000000000 (64-bit)
///
/// NOTE: Only supports RAM addresses
#[derive(clap::Parser, Serialize, Deserialize)]
#[clap(verbatim_doc_comment)]
pub struct Cmd {
    #[clap(flatten)]
    pub shared: CoreOptions,

    #[clap(flatten)]
    pub probe_options: ProbeOptions,

    #[clap(flatten)]
    pub read_write_options: ReadWriteOptions,

    /// Number of words to read from the target
    pub words: u64,
}

impl Cmd {
    pub async fn run(self, iface: &mut impl SessionInterface) -> anyhow::Result<()> {
        let sessid = iface.attach_probe(self.probe_options).await?;

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                let values = iface
                    .run_call(ReadMemory8 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        count: self.words,
                    })
                    .await?;
                for val in values {
                    print!("{:02x} ", val);
                }
            }
            ReadWriteBitWidth::B16 => {
                let values = iface
                    .run_call(ReadMemory16 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        count: self.words,
                    })
                    .await?;
                for val in values {
                    print!("{:08x} ", val);
                }
            }
            ReadWriteBitWidth::B32 => {
                let values = iface
                    .run_call(ReadMemory32 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        count: self.words,
                    })
                    .await?;
                for val in values {
                    print!("{:08x} ", val);
                }
            }
            ReadWriteBitWidth::B64 => {
                let values = iface
                    .run_call(ReadMemory64 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        count: self.words,
                    })
                    .await?;
                for val in values {
                    print!("{:016x} ", val);
                }
            }
        }
        println!();

        iface.run_call(ResumeAllCores { sessid }).await?;

        Ok(())
    }
}
