use std::path::PathBuf;

use probe_rs::flashing::FlashError;
use probe_rs::probe::list::Lister;

use crate::util::common_options::ProbeOptions;
use crate::util::flash::build_loader;
use crate::FormatOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub probe_options: ProbeOptions,

    /// The path to the file to be compared with the flash
    pub path: PathBuf,

    #[clap(flatten)]
    pub format_options: FormatOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.probe_options.simple_attach(lister)?;

        let loader = build_loader(&mut session, &self.path, self.format_options, None)?;

        match loader.verify(&mut session) {
            Ok(()) => {
                println!("Verification successful")
            }
            Err(FlashError::Verify) => println!("Verification failed"),
            Err(other) => return Err(other.into()),
        };

        Ok(())
    }
}
