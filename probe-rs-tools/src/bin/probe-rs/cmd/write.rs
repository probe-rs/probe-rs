use crate::cmd::remote::functions::write_memory::{
    WriteMemory16, WriteMemory32, WriteMemory64, WriteMemory8,
};
use crate::cmd::remote::SessionInterface;
use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
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

fn ensure_data_in_range(data: &[u64], width: ReadWriteBitWidth) -> anyhow::Result<()> {
    let max = match width {
        ReadWriteBitWidth::B8 => u8::MAX as u64,
        ReadWriteBitWidth::B16 => u16::MAX as u64,
        ReadWriteBitWidth::B32 => u32::MAX as u64,
        ReadWriteBitWidth::B64 => u64::MAX,
    };
    if let Some(big) = data.iter().find(|&&v| v > max) {
        anyhow::bail!(
            "{} in {:?} is too large for an {} bit write.",
            big,
            data,
            width as u8,
        );
    }

    Ok(())
}

impl Cmd {
    pub async fn run(self, iface: &mut impl SessionInterface) -> anyhow::Result<()> {
        let sessid = iface.attach_probe(self.probe_options).await?;

        ensure_data_in_range(&self.values, self.read_write_options.width)?;

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                iface
                    .run_call(WriteMemory8 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        data: self.values.iter().map(|v| *v as u8).collect(),
                    })
                    .await?;
            }
            ReadWriteBitWidth::B16 => {
                iface
                    .run_call(WriteMemory16 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        data: self.values.iter().map(|v| *v as u16).collect(),
                    })
                    .await?;
            }
            ReadWriteBitWidth::B32 => {
                iface
                    .run_call(WriteMemory32 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        data: self.values.iter().map(|v| *v as u32).collect(),
                    })
                    .await?;
            }
            ReadWriteBitWidth::B64 => {
                iface
                    .run_call(WriteMemory64 {
                        core: self.shared.core,
                        sessid,
                        address: self.read_write_options.address,
                        data: self.values,
                    })
                    .await?;
            }
        }

        Ok(())
    }
}
