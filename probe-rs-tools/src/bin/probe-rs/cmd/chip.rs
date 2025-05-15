use std::io::Write;

use bytesize::ByteSize;

use crate::rpc::{client::RpcClient, functions::chip::MemoryRegion};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
/// Inspect internal registry of supported chips
enum Subcommand {
    /// Lists all the available families and their chips with their full.
    #[clap(name = "list")]
    List,
    /// Shows chip properties of a specific chip
    #[clap(name = "info")]
    Info {
        /// The name of the chip to display.
        name: String,
    },
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let output = std::io::stdout().lock();

        match self.subcommand {
            Subcommand::List => print_families(&client, output).await,
            Subcommand::Info { name } => print_chip_info(&client, output, &name).await,
        }
    }
}

/// Print all the available families and their contained chips to the
/// commandline.
pub async fn print_families(client: &RpcClient, mut output: impl Write) -> anyhow::Result<()> {
    writeln!(output, "Available chips:")?;
    let families = client.list_chip_families().await?;
    for family in families {
        writeln!(output, "{}", &family.name)?;
        writeln!(output, "    Variants:")?;
        for variant in family.variants {
            writeln!(output, "        {}", variant.name)?;
        }
    }
    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub async fn print_chip_info(
    client: &RpcClient,
    mut output: impl Write,
    name: &str,
) -> anyhow::Result<()> {
    writeln!(output, "{}", name)?;
    let target = client.chip_info(name).await?;
    writeln!(output, "Cores ({}):", target.cores.len())?;
    for core in target.cores {
        writeln!(
            output,
            "    - {} ({:?})",
            core.name.to_ascii_lowercase(),
            core.core_type
        )?;
    }

    fn get_range_len(range: &std::ops::Range<u64>) -> u64 {
        range.end - range.start
    }

    for memory in target.memory_map {
        let range = memory.address_range();
        let size = ByteSize(get_range_len(&range)).display().iec();
        let kind = match memory {
            MemoryRegion::Ram(_) => "RAM",
            MemoryRegion::Generic(_) => "Generic",
            MemoryRegion::Nvm(_) => "NVM",
        };
        writeln!(output, "{kind}: {range:#010x?} ({size})")?
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use tokio::task::LocalSet;

    use super::*;
    use std::future::Future;

    async fn run_on_local_server<F, Fut>(f: F)
    where
        F: Fn(RpcClient) -> Fut,
        Fut: Future<Output = ()>,
    {
        use crate::rpc::functions::RpcApp;

        // Create a local server to run commands against.
        let (mut local_server, tx, rx) =
            RpcApp::create_server(16, crate::rpc::functions::ProbeAccess::All);
        let local = LocalSet::new();
        let handle = local.spawn_local(async move { local_server.run().await });

        // Run the command locally.
        let client = RpcClient::new_local_from_wire(tx, rx);

        f(client).await;

        // Wait for the server to shut down
        let (_, _) = tokio::join! {
            handle,
            local,
        };
    }

    #[tokio::test]
    async fn single_chip_output() {
        run_on_local_server(|client| {
            async move {
                let mut buff = Vec::new();

                print_chip_info(&client, &mut buff, "nrf52840_xxaa")
                    .await
                    .unwrap();

                // output should be valid utf8
                let output = String::from_utf8(buff).unwrap();

                insta::assert_snapshot!(output);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn multiple_chip_output() {
        run_on_local_server(|client| async move {
            let mut buff = Vec::new();

            let error = print_chip_info(&client, &mut buff, "nrf52")
                .await
                .unwrap_err();

            insta::assert_snapshot!(error.to_string());
        })
        .await;
    }
}
