use probe_rs::{probe::list::Lister, MemoryInterface};

use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::util::parse_u64;
use crate::CoreOptions;

/// Write to target memory address
///
/// e.g. probe-rs write b32 0x400E1490 0xDEADBEEF 0xCAFEF00D
///      Writes 0xDEADBEEF to address 0x400E1490 and 0xCAFEF00D to address 0x400E1494
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

    /// Values to write to the target.
    /// Takes a list of integer values and can be specified in decimal (16), hexadecimal (0x10) or octal (0o20) format.
    #[clap(value_parser = parse_u64)]
    values: Vec<u64>,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.probe_options.simple_attach(lister)?;
        let mut core = session.core(self.shared.core)?;

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                let mut bvalues = Vec::new();
                for val in &self.values {
                    if val > &(u8::MAX as u64) {
                        return Err(anyhow::anyhow!(
                            "{} in {:?} is too large for an 8 bit write.",
                            val,
                            self.values,
                        ));
                    }
                    bvalues.push(*val as u8);
                }
                core.write_8(self.read_write_options.address, &bvalues)?;
            }
            ReadWriteBitWidth::B32 => {
                let mut bvalues = Vec::new();
                for val in &self.values {
                    if val > &(u32::MAX as u64) {
                        return Err(anyhow::anyhow!(
                            "{} in {:?} is too large for a 32 bit write.",
                            val,
                            self.values,
                        ));
                    }
                    bvalues.push(*val as u32);
                }
                core.write_32(self.read_write_options.address, &bvalues)?;
            }
            ReadWriteBitWidth::B64 => {
                core.write_64(self.read_write_options.address, &self.values)?;
            }
        }

        Ok(())
    }
}
