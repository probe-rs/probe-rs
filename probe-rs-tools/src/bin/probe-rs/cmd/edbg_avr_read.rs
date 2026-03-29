use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use ihex::Record;
use probe_rs::probe::{
    DebugProbeSelector,
    cmsisdap::{AvrMemoryRegion, read_pkobn_updi_region},
    list::Lister,
};

use crate::util::common_options::{ReadWriteBitWidth, ReadWriteOptions};

use super::edbg_avr_info::select_probe_for_edbg;

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Region {
    /// Flash memory, addressed relative to the flash start.
    Flash,
    /// EEPROM memory, addressed relative to the EEPROM start.
    Eeprom,
    /// Fuse bytes, addressed relative to the fuse region start.
    Fuses,
    /// Lock byte, addressed relative to the lock region start.
    Lock,
    /// USERROW bytes, addressed relative to the user row start.
    Userrow,
    /// Production signature bytes, addressed relative to the production signature start.
    Prodsig,
}

impl From<Region> for AvrMemoryRegion {
    fn from(region: Region) -> Self {
        match region {
            Region::Flash => AvrMemoryRegion::Flash,
            Region::Eeprom => AvrMemoryRegion::Eeprom,
            Region::Fuses => AvrMemoryRegion::Fuses,
            Region::Lock => AvrMemoryRegion::Lock,
            Region::Userrow => AvrMemoryRegion::UserRow,
            Region::Prodsig => AvrMemoryRegion::ProdSig,
        }
    }
}

// TODO: refactor to share OutputFormat with read.rs
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
    fn write_to(
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

    fn save_to_file(
        self,
        address: u64,
        width: ReadWriteBitWidth,
        data: &[u8],
        path: &Path,
    ) -> anyhow::Result<()> {
        let mut buf = Vec::new();
        self.write_to(&mut buf, address, width, data)?;
        std::fs::write(path, &buf)?;
        Ok(())
    }

    fn print_to_console(
        self,
        address: u64,
        width: ReadWriteBitWidth,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let mut stdout = std::io::stdout();
        self.write_to(&mut stdout, address, width, data)
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
        let mut last_address_msbs: Option<u16> = None;

        let mut remaining = data;
        while !remaining.is_empty() {
            let address_msbs: u16 = (running_address >> 16)
                .try_into()
                .context("Hex format only supports addressing up to 32 bits")?;

            if last_address_msbs != Some(address_msbs) {
                records.push(Record::ExtendedLinearAddress(address_msbs));
                last_address_msbs = Some(address_msbs);
            }

            let bytes_until_boundary = 0x10000u64.saturating_sub(running_address & 0xFFFF);
            let chunk_len = remaining
                .len()
                .min(255)
                .min(bytes_until_boundary as usize)
                .max(1);

            records.push(Record::Data {
                offset: (running_address & 0xFFFF) as u16,
                value: remaining[..chunk_len].to_vec(),
            });
            remaining = &remaining[chunk_len..];
            running_address += chunk_len as u64;
        }

        records.push(Record::EndOfFile);
        let hexdata = ihex::create_object_file_representation(&records)?;
        dst.write_all(hexdata.as_bytes())?;
        Ok(())
    }
}

/// Experimental AVR UPDI region read.
///
/// The address argument is relative to the selected AVR region.
///
/// e.g. `probe-rs edbg-avr-read --region eeprom b8 0x00 16`
///      Reads 16 bytes from EEPROM starting at EEPROM offset 0x00.
///
/// e.g. `probe-rs edbg-avr-read --region flash b8 0x00 16`
///      Reads 16 bytes from flash starting at flash offset 0x00.
#[derive(clap::Parser)]
#[clap(verbatim_doc_comment)]
pub struct Cmd {
    /// Disable interactive probe selection
    #[arg(
        long,
        env = "PROBE_RS_NON_INTERACTIVE",
        help_heading = "PROBE CONFIGURATION"
    )]
    non_interactive: bool,
    /// Use this flag to select a specific probe in the list.
    #[arg(long, env = "PROBE_RS_PROBE", help_heading = "PROBE CONFIGURATION")]
    probe: Option<DebugProbeSelector>,

    /// AVR memory region to read from
    #[arg(long, value_enum)]
    region: Region,

    #[clap(flatten)]
    read_write_options: ReadWriteOptions,

    /// Number of words to read from the selected region
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
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let probe = select_probe_for_edbg(lister, self.probe.as_ref(), self.non_interactive)?;
        let selector = DebugProbeSelector::from(&probe);
        let region: AvrMemoryRegion = self.region.into();
        let byte_len = self
            .words
            .checked_mul(match self.read_write_options.width {
                ReadWriteBitWidth::B8 => 1,
                ReadWriteBitWidth::B16 => 2,
                ReadWriteBitWidth::B32 => 4,
                ReadWriteBitWidth::B64 => 8,
            })
            .context("requested read length overflowed")?;
        let byte_len =
            u32::try_from(byte_len).context("requested read length exceeds 32-bit range")?;
        anyhow::ensure!(
            byte_len > 0,
            "requested read length must be greater than zero"
        );
        let offset = u32::try_from(self.read_write_options.address)
            .context("region-relative AVR address exceeds 32-bit range")?;

        let data = read_pkobn_updi_region(&selector, region, offset, byte_len)?;

        match self.output {
            Some(path) => self.format.save_to_file(
                self.read_write_options.address,
                self.read_write_options.width,
                &data,
                &path,
            )?,
            None => self.format.print_to_console(
                self.read_write_options.address,
                self.read_write_options.width,
                &data,
            )?,
        }

        Ok(())
    }
}
