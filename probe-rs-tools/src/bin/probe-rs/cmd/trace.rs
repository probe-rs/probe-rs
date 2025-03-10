use std::io::prelude::*;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use probe_rs::MemoryInterface;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use scroll::{LE, Pwrite};

use crate::CoreOptions;
use crate::util::{common_options::ProbeOptions, parse_u64};

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
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let mut xs = vec![];
        let mut ys = vec![];

        let start = Instant::now();

        let (mut session, _probe_options) = self.common.simple_attach(registry, lister)?;

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
