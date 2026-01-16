use crate::{
    rpc::{client::RpcClient, functions::flash::EraseCommand},
    util::{cli, common_options::ProbeOptions, flash::CliProgressBars},
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    #[arg(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub disable_progressbars: bool,

    /// Whether to read the RTT output from the flash loader, if available.
    #[clap(long)]
    pub read_flasher_rtt: bool,

    // TODO: I did not manage to get clap to use an enum like `pub enum Mode { All,
    // Range(Range<u64>) }` for that. This would eliminate the need for convoluted condition checks
    // when processing the command.
    /// Erase all nonvolatile memory.
    #[arg(long, group = "mode")]
    pub all: bool,
    /// Erase the nonvolatile menory pages containing this address range (an exclusive
    /// range like START..END where END is not included).
    // TODO: What about usind `parse_int::range::Range` as range type and allowing to specify more
    // variants than just closed exclusive ranges.
    #[arg(long, group = "mode", value_parser = parse_erase_range)]
    pub range: Option<std::ops::Range<u64>>,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, false).await?;

        let pb = if self.disable_progressbars {
            None
        } else {
            Some(CliProgressBars::new())
        };

        if let Some(range) = self.range {
            session
                // TODO: There is currently no progress ouput from erasing a range. Add some to the
                // handler of this erase command.
                .erase(
                    EraseCommand::Range(range),
                    self.read_flasher_rtt,
                    async move |event| {
                        if let Some(pb) = pb.as_ref() {
                            pb.handle(event);
                        }
                    },
                )
                .await?;
        } else {
            // TODO: Remove erasing all nonvolatile memory by default.
            // Erasing the entire nonvolatile memory has been the default historically.
            if !self.all {
                tracing::warn!(
                    "Defaulting to erasing all nonvolatile memory. Please specify '--all' in the future."
                );
            }

            session
                .erase(
                    EraseCommand::All,
                    self.read_flasher_rtt,
                    async move |event| {
                        if let Some(pb) = pb.as_ref() {
                            pb.handle(event);
                        }
                    },
                )
                .await?;
        }

        Ok(())
    }
}

fn parse_erase_range(s: &str) -> Result<std::ops::Range<u64>, String> {
    parse_int::range::parse_range::<u64>(s)
        // TODO: Improve error message generation for RangeParsingError.
        .map_err(|e| format!("Invalid range ({e:?}). Expecting an exclusive range 'start..end'."))?
        .as_range_exclusive()
        .ok_or_else(|| String::from("Exclusive range 'start..end' required."))
}
