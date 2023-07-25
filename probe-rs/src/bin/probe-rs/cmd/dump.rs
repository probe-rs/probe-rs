use std::time::Instant;

use probe_rs::MemoryInterface;

use crate::util::{common_options::ProbeOptions, parse_u32, parse_u64};
use crate::CoreOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// The address of the memory to dump from the target.
    #[clap(value_parser = parse_u64)]
    loc: u64,

    /// The amount of memory (in words) to dump.
    #[clap(value_parser = parse_u32)]
    words: u32,
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach()?;

        let mut data = vec![0_u32; self.words as usize];

        // Start timer.
        let instant = Instant::now();

        // let loc = 220 * 1024;

        let mut core = session.core(self.shared.core)?;

        core.read_32(self.loc, data.as_mut_slice())?;
        // Stop timer.
        let elapsed = instant.elapsed();

        // Print read values.
        let words = self.words;
        for word in 0..words {
            println!(
                "Addr 0x{:08x?}: 0x{:08x}",
                self.loc + 4 * word as u64,
                data[word as usize]
            );
        }
        // Print stats.
        println!("Read {words:?} words in {elapsed:?}");

        Ok(())
    }
}
