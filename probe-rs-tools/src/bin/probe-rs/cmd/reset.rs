use probe_rs::probe::list::Lister;

use crate::{util::common_options::ProbeOptions, CoreOptions};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister).await?;

        session.core(self.shared.core).await?.reset().await?;

        Ok(())
    }
}
