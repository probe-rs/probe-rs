use probe_rs::flashing::erase_all;

use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let mut session = self.common.simple_attach()?;

        erase_all(&mut session, None)?;

        Ok(())
    }
}
