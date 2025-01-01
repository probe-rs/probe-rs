use serde::{Deserialize, Serialize};

use crate::{
    cmd::remote::{
        functions::{list_probes::DebugProbeEntry, RemoteFunctions},
        LocalSession, SessionId, SessionInterface,
    },
    util::common_options::{OperationError, ProbeOptions},
};

#[derive(Serialize, Deserialize)]
pub enum AttachResult {
    Success(SessionId),
    MultipleProbes(Vec<DebugProbeEntry>),
}

#[derive(Serialize, Deserialize)]
pub struct Attach {
    pub probe_options: ProbeOptions,
}

impl super::RemoteFunction for Attach {
    type Result = AttachResult;

    async fn run(mut self, iface: &mut LocalSession) -> Self::Result {
        self.probe_options.non_interactive = true;

        match self.probe_options.simple_attach(&iface.lister()) {
            Ok((session, _)) => {
                let session_id = iface.set_session(session);
                AttachResult::Success(session_id)
            }
            Err(OperationError::MultipleProbesFound { list }) => {
                AttachResult::MultipleProbes(list.into_iter().map(DebugProbeEntry::from).collect())
            }
            Err(other) => panic!("Unexpected error: {:?}", other),
        }
    }
}

impl From<Attach> for RemoteFunctions {
    fn from(func: Attach) -> Self {
        RemoteFunctions::Attach(func)
    }
}

pub(in crate::cmd::remote) async fn attach_probe(
    probe_options: ProbeOptions,
    iface: &mut impl SessionInterface,
) -> anyhow::Result<SessionId> {
    use anyhow::Context as _;
    use std::io::Write as _;

    let result = iface
        .run_call(Attach {
            probe_options: probe_options.clone(),
        })
        .await?;

    match result {
        AttachResult::Success(sessid) => Ok(sessid),
        AttachResult::MultipleProbes(list) => {
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

            let mut probe_options = probe_options.clone();
            probe_options.probe = Some(probe.selector());

            match iface.run_call(Attach { probe_options }).await? {
                AttachResult::Success(session_id) => Ok(session_id),
                AttachResult::MultipleProbes(_) => {
                    anyhow::bail!("Did not expect multiple probes")
                }
            }
        }
    }
}
