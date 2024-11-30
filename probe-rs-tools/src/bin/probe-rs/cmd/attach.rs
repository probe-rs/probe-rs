use probe_rs::probe::list::Lister;
use time::UtcOffset;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) run: crate::cmd::run::Cmd,
}

impl Cmd {
    pub async fn run(self, lister: &Lister, timestamp_offset: UtcOffset) -> anyhow::Result<()> {
        self.run.run(lister, false, timestamp_offset).await?;

        Ok(())
    }
}
