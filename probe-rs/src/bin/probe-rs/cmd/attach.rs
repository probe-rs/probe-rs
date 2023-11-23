use probe_rs::ProbeLister;
use time::UtcOffset;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) run: crate::cmd::run::Cmd,
}

impl Cmd {
    pub fn run(self, lister: &impl ProbeLister, timestamp_offset: UtcOffset) -> anyhow::Result<()> {
        self.run.run(lister, false, timestamp_offset)?;

        Ok(())
    }
}
