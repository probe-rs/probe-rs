use probe_rs::probe::list::Lister;
use serde::{Deserialize, Serialize};

use crate::{util::common_options::ProbeOptions, CoreOptions};

#[derive(clap::Parser, Serialize, Deserialize)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister)?;

        session.core(self.shared.core)?.reset()?;

        Ok(())
    }
}
