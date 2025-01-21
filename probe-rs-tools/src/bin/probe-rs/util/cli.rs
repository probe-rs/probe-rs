//! CLI-specific building blocks.

use crate::{
    rpc::{
        client::{RpcClient, SessionInterface},
        functions::probe::{
            AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeResult,
        },
    },
    util::common_options::ProbeOptions,
};

pub async fn attach_probe(
    client: &RpcClient,
    probe_options: ProbeOptions,
    resume_target: bool,
) -> anyhow::Result<SessionInterface> {
    let probe = select_probe(client, probe_options.probe.map(Into::into)).await?;

    let result = client
        .attach_probe(AttachRequest {
            chip: probe_options.chip,
            protocol: probe_options.protocol.map(Into::into),
            probe,
            speed: probe_options.speed,
            connect_under_reset: probe_options.connect_under_reset,
            dry_run: probe_options.dry_run,
            allow_erase_all: probe_options.allow_erase_all,
            resume_target,
        })
        .await?;

    match result {
        AttachResult::Success(sessid) => Ok(SessionInterface::new(client.clone(), sessid)),
        AttachResult::ProbeNotFound => anyhow::bail!("Probe not found"),
        AttachResult::ProbeInUse => anyhow::bail!("Probe is already in use"),
    }
}

pub async fn select_probe(
    client: &RpcClient,
    probe: Option<DebugProbeSelector>,
) -> anyhow::Result<DebugProbeEntry> {
    use anyhow::Context as _;
    use std::io::Write as _;

    match client.select_probe(probe).await? {
        SelectProbeResult::Success(probe) => Ok(probe),
        SelectProbeResult::MultipleProbes(list) => {
            println!("Available Probes:");
            for (i, probe_info) in list.iter().enumerate() {
                println!("{i}: {probe_info}");
            }

            print!("Selection: ");
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .expect("Expect input for probe selection");

            let probe_idx = input
                .trim()
                .parse::<usize>()
                .context("Failed to parse probe index")?;

            let probe = list
                .get(probe_idx)
                .ok_or_else(|| anyhow::anyhow!("Probe not found"))?;

            match client.select_probe(Some(probe.selector())).await? {
                SelectProbeResult::Success(probe) => Ok(probe),
                SelectProbeResult::MultipleProbes(_) => {
                    anyhow::bail!("Did not expect multiple probes")
                }
            }
        }
    }
}
