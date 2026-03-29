use std::path::PathBuf;

use anyhow::Context;

use crate::FormatOptions;
use crate::rpc::client::RpcClient;
use crate::rpc::functions::flash::VerifyResult;
use crate::util::cli;
use crate::util::common_options::ProbeOptions;
use crate::util::flash::CliProgressBars;
use probe_rs::probe::WireProtocol;

use super::download::load_updi_flash_blocks;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub probe_options: ProbeOptions,

    /// The path to the file to be compared with the flash
    pub path: PathBuf,

    #[clap(flatten)]
    pub format_options: FormatOptions,

    #[clap(long)]
    pub disable_progressbars: bool,

    /// Whether to read the RTT output from the flash loader, if available.
    #[clap(long)]
    pub read_flasher_rtt: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        if self.probe_options.protocol == Some(WireProtocol::Updi) {
            self.run_updi_verify(&client).await
        } else {
            let session = cli::attach_probe(&client, self.probe_options, false).await?;

            let pb = if self.disable_progressbars {
                None
            } else {
                Some(CliProgressBars::new())
            };
            let loader = session
                .build_flash_loader(
                    self.path.to_path_buf(),
                    self.format_options,
                    None,
                    self.read_flasher_rtt,
                )
                .await?;

            let result = session
                .verify(loader.loader, async move |event| {
                    if let Some(pb) = pb.as_ref() {
                        pb.handle(event);
                    }
                })
                .await?;

            match result {
                VerifyResult::Ok => println!("Verification successful"),
                VerifyResult::Mismatch => println!("Verification failed: contents do not match"),
            }

            Ok(())
        }
    }

    async fn run_updi_verify(self, client: &RpcClient) -> anyhow::Result<()> {
        if self.read_flasher_rtt {
            anyhow::bail!("'verify --protocol updi' does not support '--read-flasher-rtt'.");
        }
        if !client.is_local_session() {
            anyhow::bail!(
                "The protocol 'UPDI' is currently only supported by 'verify' in a local session."
            );
        }

        let session = cli::attach_probe(client, self.probe_options, false).await?;
        let core = session.core(0);
        let blocks = load_updi_flash_blocks(&self.path, &self.format_options)?;

        if blocks.is_empty() {
            anyhow::bail!("No flashable data found in '{}'.", self.path.display());
        }

        for block in &blocks {
            let readback = core
                .read_memory_8(
                    u64::from(block.address),
                    u32::try_from(block.data.len())
                        .context("flash block length exceeds 32-bit range")?
                        as usize,
                )
                .await?;
            if readback != block.data {
                anyhow::bail!(
                    "Verification failed: contents do not match at flash offset 0x{:04x}",
                    block.address
                );
            }
        }

        println!("Verification successful");
        Ok(())
    }
}
