use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use time::UtcOffset;

use crate::{Cli, Config};

mod meta;

#[derive(clap::Parser, Serialize, Deserialize)]
#[group(skip)]
pub struct Cmd {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand, Serialize, Deserialize)]
#[group(skip)]
pub enum Subcommand {
    Meta,
    Cli(CliJson),
}

#[derive(clap::Parser, Serialize, Deserialize)]
pub struct CliJson {
    json: String,
}

impl Cmd {
    pub async fn run(self, config: Config, utc_offset: UtcOffset) -> anyhow::Result<()> {
        match self.subcommand {
            Subcommand::Meta => meta::run()?,
            Subcommand::Cli(cli_json) => {
                let mut cli = serde_json::from_str::<Cli>(&cli_json.json)
                    .context("Failed to parse command")?;
                if let Some(probe_options) = cli.subcommand.probe_options_mut() {
                    // FIXME: this is a workaround, we don't support stdin in the remote client
                    probe_options.non_interactive = true;
                }
                let fut = cli.run(config, utc_offset);
                Box::pin(fut).await?;
            }
        }
        Ok(())
    }
}
