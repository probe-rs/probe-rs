use probe_rs::{probe::list::Lister, MemoryInterface};

use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::CoreOptions;

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
    words: u64,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.probe_options.simple_attach(lister)?;

        let mut core = session.core(self.shared.core)?;
        let words = self.words as usize;

        match self.read_write_options.width {
            ReadWriteBitWidth::B8 => {
                let mut values = vec![0; words];
                core.read_8(self.read_write_options.address, &mut values)?;
                for val in values {
                    print!("{:02x} ", val);
                }
                println!();
            }
            ReadWriteBitWidth::B32 => {
                let mut values = vec![0; words];
                core.read_32(self.read_write_options.address, &mut values)?;
                for val in values {
                    print!("{:08x} ", val);
                }
                println!();
            }
            ReadWriteBitWidth::B64 => {
                let mut values = vec![0; words];
                core.read_64(self.read_write_options.address, &mut values)?;
                for val in values {
                    print!("{:016x} ", val);
                }
                println!();
            }
        }
        std::mem::drop(core);

        session.resume_all_cores()?;

        Ok(())
    }
}
