use std::io::Write;
use std::path::Path;
use std::time::Duration;

use time::UtcOffset;

use crate::util::common_options::ProbeOptions;
use crate::util::rtt;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) common: ProbeOptions,

    /// The path to the ELF file to flash and run
    pub(crate) path: String,
}

impl Cmd {
    pub fn run(self, timestamp_offset: UtcOffset) -> anyhow::Result<()> {
        let mut session = self.common.simple_attach()?;

        let rtt_config = rtt::RttConfig::default();

        let memory_map = session.target().memory_map.clone();

        let mut core = session.core(0)?;

        match rtt::attach_to_rtt(
            &mut core,
            &memory_map,
            Path::new(&self.path),
            &rtt_config,
            timestamp_offset,
        ) {
            Ok(mut rtta) => {
                let mut stdout = std::io::stdout();
                loop {
                    for (_ch, data) in rtta.poll_rtt_fallible(&mut core)? {
                        stdout.write_all(data.as_bytes())?;
                    }

                    // Poll RTT with a frequency of 10 Hz
                    //
                    // If the polling frequency is too high,
                    // the USB connection to the probe can become unstable.
                    std::thread::sleep(Duration::from_millis(100));
                }
            },
            Err(error) => {
                log::error!("{:?} RTT is not available.", error);
            }
        };

        Ok(())
    }
}
