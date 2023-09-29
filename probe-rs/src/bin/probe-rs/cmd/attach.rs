use time::UtcOffset;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) run: crate::cmd::run::Cmd,
}

impl Cmd {
    pub fn run(self, timestamp_offset: UtcOffset) -> anyhow::Result<()> {
        self.run.run(false, timestamp_offset)?;

        Ok(())
    }
}
