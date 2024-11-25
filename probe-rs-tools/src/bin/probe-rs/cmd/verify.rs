use std::path::PathBuf;

use probe_rs::flashing::{FlashCommitInfo, FlashError};
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
        let mut commit_info = FlashCommitInfo::default();

        match loader.verify(&mut session, &mut commit_info) {
            Ok(()) => {
                println!("Verification successful")
            }
            Err(FlashError::Verify) => println!("Verification failed"),
            Err(other) => return Err(other.into()),
        };

        Ok(())
    }
}
