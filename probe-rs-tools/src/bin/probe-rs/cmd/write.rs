use crate::cmd::remote::functions::write_memory::WriteMemory;
use crate::cmd::remote::SessionInterface;
use crate::util::common_options::{ProbeOptions, ReadWriteOptions};
use crate::util::parse_u64;
use crate::CoreOptions;
use serde::{Deserialize, Serialize};

/// Write to target memory address
///
/// e.g. probe-rs write b32 0x400E1490 0xDEADBEEF 0xCAFEF00D
///      Writes 0xDEADBEEF to address 0x400E1490 and 0xCAFEF00D to address 0x400E1494
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

    /// Values to write to the target.
    /// Takes a list of integer values and can be specified in decimal (16), hexadecimal (0x10) or octal (0o20) format.
    #[clap(value_parser = parse_u64)]
    pub values: Vec<u64>,
}

impl Cmd {
    pub async fn run(self, iface: &mut impl SessionInterface) -> anyhow::Result<()> {
        let sessid = iface.attach_probe(self.probe_options).await?;

        iface
            .run_call(WriteMemory {
                core: self.shared.core,
                sessid,
                address: self.read_write_options.address,
                data: self.values,
                width: self.read_write_options.width,
            })
            .await?;

        Ok(())
    }
}
