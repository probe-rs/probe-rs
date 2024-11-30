mod meta;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
#[group(skip)]
pub enum Subcommand {
    Meta,
}

impl Cmd {
    pub async fn run(self) -> anyhow::Result<()> {
        match self.subcommand {
            Subcommand::Meta => meta::run()?,
        }
        Ok(())
    }
}
