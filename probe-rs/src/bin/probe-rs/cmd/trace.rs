use std::time::Instant;

use probe_rs::MemoryInterface;

use crate::util::{common_options::ProbeOptions, parse_u64};
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
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        use std::io::prelude::*;
        use std::thread::sleep;
        use std::time::Duration;

        use scroll::{Pwrite, LE};

        let mut xs = vec![];
        let mut ys = vec![];

        let start = Instant::now();

        let mut session = self.common.simple_attach()?;

        let mut core = session.core(self.shared.core)?;

        loop {
            // Prepare read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

            // Read data.
            let value: u32 = core.read_word_32(self.loc)?;

            xs.push(instant);
            ys.push(value);

            // Send value to plot.py.
            let mut buf = [0_u8; 8];
            // Unwrap is safe!
            buf.pwrite_with(instant, 0, LE).unwrap();
            buf.pwrite_with(value, 4, LE).unwrap();
            std::io::stdout().write_all(&buf)?;

            std::io::stdout().flush()?;

            // Schedule next read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());
            let poll_every_ms = 50;
            let time_to_wait = poll_every_ms - instant % poll_every_ms;
            sleep(Duration::from_millis(time_to_wait));
        }
    }
}
