use anyhow::Context;
use ihex::Record;
use itertools::Itertools;

use crate::rpc::client::RpcClient;

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
/// Output is a space separated list of hex values padded to the read word width.
/// e.g. 2 words
///     00 00 (8-bit)
///     00000000 00000000 (32-bit)
///     0000000000000000 0000000000000000 (64-bit)
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
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, false).await?;
        let core = session.core(self.shared.core);

        let nbytes;
        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                let values = core
                    .read_memory_8(self.read_write_options.address, self.words)
                    .await?;
                for val in values {
                    print!("{:02x} ", val);
                }
                nbytes = self.words;
            }
            ReadWriteBitWidth::B16 => {
                let values = core
                    .read_memory_16(self.read_write_options.address, self.words)
                    .await?;
                for val in values {
                    print!("{:08x} ", val);
                }
                nbytes = self.words * 2;
            }
            ReadWriteBitWidth::B32 => {
                let values = core
                    .read_memory_32(self.read_write_options.address, self.words)
                    .await?;
                for val in values {
                    print!("{:08x} ", val);
                }
                nbytes = self.words * 4;
            }
            ReadWriteBitWidth::B64 => {
                let values = core
                    .read_memory_64(self.read_write_options.address, self.words)
                    .await?;
                for val in values {
                    print!("{:016x} ", val);
                }
                nbytes = self.words * 8;
            }
        }

        if let Some(path) = self.output {
            let mut running_address = self.read_write_options.address;
            // Read a fresh set of data from the chip at the requested location.
            // We can't reuse the prior data because we don't know how to handle
            // endianness
            let data = core
                .read_memory_8(self.read_write_options.address, nbytes)
                .await?;

            match self.format {
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
        }
        println!();

        session.resume_all_cores().await?;

        Ok(())
    }
}
