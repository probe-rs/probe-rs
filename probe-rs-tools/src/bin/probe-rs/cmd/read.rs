use anyhow::Context;
use ihex::Record;
use itertools::Itertools;

use crate::rpc::client::{CoreInterface, RpcClient};

use crate::CoreOptions;
use crate::util::cli;
use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::ValueEnum, Clone)]
enum FileFormat {
    Hex,
    Binary,
}

/// Read from target memory address
///
/// e.g. probe-rs read b32 0x400E1490 2
///      Reads 2 32-bit words from address 0x400E1490
///
/// Default output is a space separated list of hex values padded to the read word width.
/// e.g. 2 words
///     00 00 (8-bit)
///     00000000 00000000 (32-bit)
///     0000000000000000 0000000000000000 (64-bit)
///
/// If the --output argument is provided, readback data is instead saved to a file as hex/bin.
/// In this case, the read word width has no effect except determining the total number of bytes
///
/// NOTE: Only supports RAM addresses
#[derive(clap::Parser)]
#[clap(verbatim_doc_comment)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    probe_options: ProbeOptions,

    #[clap(flatten)]
    read_write_options: ReadWriteOptions,

    /// Number of words to read from the target
    words: usize,
    /// File to output binary data to
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Format of the outputted binary data
    #[clap(value_enum, default_value_t=FileFormat::Hex)]
    #[arg(long, short, requires("output"))]
    format: FileFormat,
    /// Output as JSON for programmatic consumption
    #[arg(long, conflicts_with("output"))]
    json: bool,
}

impl Cmd {
    async fn read_to_console(
        core: CoreInterface,
        address: u64,
        width: ReadWriteBitWidth,
        nwords: usize,
    ) -> anyhow::Result<()> {
        match width {
            ReadWriteBitWidth::B8 => {
                let values = core.read_memory_8(address, nwords).await?;
                for val in values {
                    print!("{:02x} ", val);
                }
            }
            ReadWriteBitWidth::B16 => {
                let values = core.read_memory_16(address, nwords).await?;
                for val in values {
                    print!("{:08x} ", val);
                }
            }
            ReadWriteBitWidth::B32 => {
                let values = core.read_memory_32(address, nwords).await?;
                for val in values {
                    print!("{:08x} ", val);
                }
            }
            ReadWriteBitWidth::B64 => {
                let values = core.read_memory_64(address, nwords).await?;
                for val in values {
                    print!("{:016x} ", val);
                }
            }
        }
        println!();
        Ok(())
    }

    async fn read_to_file(
        core: CoreInterface,
        address: u64,
        width: ReadWriteBitWidth,
        nwords: usize,
        path: PathBuf,
        format: FileFormat,
    ) -> anyhow::Result<()> {
        let nbytes = nwords
            * match width {
                ReadWriteBitWidth::B8 => 1,
                ReadWriteBitWidth::B16 => 2,
                ReadWriteBitWidth::B32 => 4,
                ReadWriteBitWidth::B64 => 8,
            };

        let data = core.read_memory_8(address, nbytes).await?;

        let mut running_address = address;
        match format {
            FileFormat::Binary => {
                std::fs::File::create(path)?.write_all(&data)?;
            }
            FileFormat::Hex => {
                let mut records = vec![];

                for chunk in &data.into_iter().chunks(255) {
                    let address_msbs: u16 = (running_address >> 16)
                        .try_into()
                        .context("Hex format only supports addressing up to 32 bits")?;

                    records.push(Record::ExtendedLinearAddress(address_msbs));

                    records.push(Record::Data {
                        offset: (running_address & 0xFFFF) as u16,
                        value: chunk.collect(),
                    });
                    running_address += 255;
                }
                records.push(Record::EndOfFile);
                let hexdata = ihex::create_object_file_representation(&records)?;
                std::fs::File::create(path)?.write_all(hexdata.as_bytes())?;
            }
        }
        Ok(())
    }

    async fn read_to_json(
        core: CoreInterface,
        address: u64,
        width: ReadWriteBitWidth,
        nwords: usize,
    ) -> anyhow::Result<()> {
        let (width_bits, values) = match width {
            ReadWriteBitWidth::B8 => {
                let v = core.read_memory_8(address, nwords).await?;
                (8u32, v.into_iter().map(|x| x as u64).collect::<Vec<_>>())
            }
            ReadWriteBitWidth::B16 => {
                let v = core.read_memory_16(address, nwords).await?;
                (16, v.into_iter().map(|x| x as u64).collect())
            }
            ReadWriteBitWidth::B32 => {
                let v = core.read_memory_32(address, nwords).await?;
                (32, v.into_iter().map(|x| x as u64).collect())
            }
            ReadWriteBitWidth::B64 => {
                let v = core.read_memory_64(address, nwords).await?;
                (64, v)
            }
        };
        let out = serde_json::json!({
            "addr": format!("{address:#010x}"),
            "width": width_bits,
            "values": values,
        });
        println!("{}", serde_json::to_string(&out)?);
        Ok(())
    }

    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, false).await?;
        let core = session.core(self.shared.core);

        if self.json {
            Cmd::read_to_json(
                core,
                self.read_write_options.address,
                self.read_write_options.width,
                self.words,
            )
            .await?;
        } else {
            match self.output {
                Some(path) => {
                    Cmd::read_to_file(
                        core,
                        self.read_write_options.address,
                        self.read_write_options.width,
                        self.words,
                        path,
                        self.format,
                    )
                    .await?
                }
                None => {
                    Cmd::read_to_console(
                        core,
                        self.read_write_options.address,
                        self.read_write_options.width,
                        self.words,
                    )
                    .await?
                }
            }
        }

        session.resume_all_cores().await?;

        Ok(())
    }
}
