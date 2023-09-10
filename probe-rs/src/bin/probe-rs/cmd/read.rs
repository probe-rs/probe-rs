use probe_rs::MemoryInterface;

use crate::util::common_options::{ProbeOptions, ReadWriteBitWidth, ReadWriteOptions};
use crate::CoreOptions;

#[derive(clap::Parser)]
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
    pub fn run(self) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.probe_options.simple_attach()?;
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

        Ok(())
    }
}
