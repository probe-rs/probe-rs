use anyhow::Context;
use ihex::Record;
use itertools::Itertools;

use crate::rpc::client::{CoreInterface, RpcClient};

use crate::CoreOptions;
use crate::util::cli;
use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::ValueEnum, Clone, Copy)]
enum OutputFormat {
    /// Intel Hex Format
    Ihex,
    /// Simple list of hexadecimal numbers
    SimpleHex,
    /// Hexadecimal numbers formatted into a table
    HexTable,
    /// The raw binary
    Binary,
}

impl OutputFormat {
    fn write(
        self,
        dst: impl Write,
        address: u64,
        width: ReadWriteBitWidth,
        data: &[u8],
    ) -> anyhow::Result<()> {
        match self {
            OutputFormat::Binary => Self::write_binary(dst, data),
            OutputFormat::Ihex => Self::write_ihex(dst, address, data),
            OutputFormat::SimpleHex => Self::write_simple_hex(dst, width, data),
            OutputFormat::HexTable => Self::write_hex_table(dst, address, width, data),
        }
    }

    fn write_simple_hex(
        mut dst: impl Write,
        width: ReadWriteBitWidth,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let bytes = match width {
            ReadWriteBitWidth::B8 => 1,
            ReadWriteBitWidth::B16 => 2,
            ReadWriteBitWidth::B32 => 4,
            ReadWriteBitWidth::B64 => 8,
        };

        let mut first = true;
        for window in data.chunks(bytes) {
            if first {
                first = false;
            } else {
                write!(dst, " ")?;
            }

            for byte in window.iter().rev() {
                write!(dst, "{byte:02x}")?;
            }
        }

        writeln!(dst)?;
        Ok(())
    }

    fn write_hex_table(
        mut dst: impl Write,
        mut address: u64,
        width: ReadWriteBitWidth,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let bytes_in_line = match width {
            ReadWriteBitWidth::B8 => 8,
            ReadWriteBitWidth::B16 => 16,
            ReadWriteBitWidth::B32 | ReadWriteBitWidth::B64 => 32,
        };
        for window in data.chunks(bytes_in_line) {
            write!(dst, "{address:08x}: ")?;
            Self::write_simple_hex(&mut dst, width, window)?;
            address += bytes_in_line as u64;
        }

        Ok(())
    }

    fn write_binary(mut dst: impl Write, data: &[u8]) -> anyhow::Result<()> {
        dst.write_all(data)?;

        Ok(())
    }

    fn write_ihex(mut dst: impl Write, address: u64, data: &[u8]) -> anyhow::Result<()> {
        let mut running_address = address;
        let mut records = vec![];

        for chunk in &data.iter().copied().chunks(255) {
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
        dst.write_all(hexdata.as_bytes())?;

        Ok(())
    }
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
    #[clap(value_enum, default_value_t=OutputFormat::HexTable)]
    #[arg(long, short)]
    format: OutputFormat,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, false).await?;
        let core = session.core(self.shared.core);

        let data = Self::read_memory(
            core,
            self.read_write_options.address,
            self.read_write_options.width,
            self.words,
        )
        .await?;

        match self.output {
            Some(path) => Self::save_to_file(
                self.read_write_options.address,
                &data,
                path,
                self.read_write_options.width,
                self.format,
            )?,
            None => Self::print_to_console(
                self.read_write_options.address,
                &data,
                self.read_write_options.width,
                self.format,
            )?,
        };

        session.resume_all_cores().await?;

        Ok(())
    }

    async fn read_memory(
        core: CoreInterface,
        address: u64,
        width: ReadWriteBitWidth,
        nwords: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let bytes = match width {
            ReadWriteBitWidth::B8 => {
                let values = core.read_memory_8(address, nwords).await?;
                values
                    .into_iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>()
            }
            ReadWriteBitWidth::B16 => {
                let values = core.read_memory_16(address, nwords).await?;
                values
                    .into_iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>()
            }
            ReadWriteBitWidth::B32 => {
                let values = core.read_memory_32(address, nwords).await?;
                values
                    .into_iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>()
            }
            ReadWriteBitWidth::B64 => {
                let values = core.read_memory_64(address, nwords).await?;
                values
                    .into_iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>()
            }
        };
        Ok(bytes)
    }

    fn save_to_file(
        address: u64,
        data: &[u8],
        path: PathBuf,
        width: ReadWriteBitWidth,
        format: OutputFormat,
    ) -> anyhow::Result<()> {
        let mut file = std::fs::File::create(path)?;
        format.write(&mut file, address, width, data)?;
        Ok(())
    }

    fn print_to_console(
        address: u64,
        data: &[u8],
        width: ReadWriteBitWidth,
        format: OutputFormat,
    ) -> anyhow::Result<()> {
        let mut stdout = std::io::stdout();
        format.write(&mut stdout, address, width, data)?;
        Ok(())
    }
}
