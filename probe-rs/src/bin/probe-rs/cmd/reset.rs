use crate::{util::common_options::ProbeOptions, CoreOptions};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
    assert: Option<bool>,
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach()?;

        session.core(self.shared.core)?.reset()?;

        Ok(())
    }
}
