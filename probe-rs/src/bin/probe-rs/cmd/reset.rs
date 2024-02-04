use probe_rs::{probe::list::Lister, CoreSelector};

use crate::{util::common_options::ProbeOptions, CoreOptions};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self
            .common
            .simple_attach(lister, &CoreSelector::default())?;

        session.core(self.shared.core)?.reset()?;

        Ok(())
    }
}
