//! Per-DAP-client RPC connection lifetime.
//!
//! TCP multi-session mode creates one RPC connection (local in-process server
//! or remote websocket client) for each accepted DAP client. Dropping the
//! connection tears down all connection-scoped server state: sessions, debug
//! state, RTT clients, flash loaders, temp files, and the client upload cache.

#[cfg(feature = "remote")]
use anyhow::Context;
use anyhow::Result;
use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::rpc::functions::{ProbeAccess, RpcApp};
#[cfg(feature = "remote")]
use probe_rs_rpc_client::connect;
use probe_rs_rpc_client::{RemoteParams, RpcClient};

/// Owns one RPC client and, for local mode, the in-process server task backing
/// it. Drop or [`close`](Self::close) to release connection-scoped state.
pub(crate) struct DapRpcConnection {
    client: Option<RpcClient>,
    local_server: Option<JoinHandle<()>>,
}

impl DapRpcConnection {
    /// Open a fresh RPC connection for one DAP client session.
    pub(crate) async fn open(
        #[cfg_attr(not(feature = "remote"), expect(unused))] remote: &RemoteParams,
    ) -> Result<Self> {
        #[cfg(feature = "remote")]
        if let Some((host, token)) = remote {
            let client = connect(host, token.as_deref(), &crate::util::meta::rpc_user_agent())
                .await
                .context("Failed to connect to remote probe-rs server")?;
            return Ok(Self {
                client: Some(client),
                local_server: None,
            });
        }

        let probe_broker = Arc::new(crate::rpc::probe_broker::ProbeBroker::new());
        let (mut local_server, tx, rx) = RpcApp::create_server(16, ProbeAccess::All, probe_broker);
        let handle = tokio::spawn(async move {
            local_server.run().await;
        });
        let client = RpcClient::new_local_from_wire(tx, rx);
        Ok(Self {
            client: Some(client),
            local_server: Some(handle),
        })
    }

    pub(crate) fn client(&self) -> &RpcClient {
        #[expect(
            clippy::expect_used,
            reason = "client is only cleared in close(); callers must not use the handle after that"
        )]
        {
            self.client
                .as_ref()
                .expect("DapRpcConnection::client called after close")
        }
    }

    /// Close the RPC connection and wait for a local in-process server to exit.
    pub(crate) async fn close(mut self) {
        self.client.take();
        if let Some(handle) = self.local_server.take() {
            let _ = handle.await;
        }
    }
}

/// Run `f` against a fresh RPC connection, ensuring it is torn down on all
/// normal and error exits from `f`.
pub(crate) async fn with_dap_rpc_connection<R>(
    remote: &RemoteParams,
    f: impl AsyncFnOnce(&RpcClient) -> Result<R>,
) -> Result<R> {
    let conn = DapRpcConnection::open(remote).await?;
    let result = f(conn.client()).await;
    conn.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::dap_server::test::{MockProbeFactory, TestLister};
    use crate::util::common_options::ProbeOptions;
    use probe_rs::architecture::arm::FullyQualifiedApAddress;
    use probe_rs::integration::{FakeProbe, Operation};
    use probe_rs::probe::DebugProbeInfo;
    use std::path::PathBuf;
    use std::sync::Arc;

    const TEST_CHIP_NAME: &str = "nRF52833_xxAA";

    fn program_binary() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../probe-rs-debug/tests/debug-unwind-tests/nRF52833_xxAA_full_unwind.elf")
    }

    fn fake_probe(serial: &str) -> (DebugProbeInfo, FakeProbe) {
        let probe_info = DebugProbeInfo::new(
            "Mock probe",
            0x12,
            0x23,
            Some(serial.to_owned()),
            &MockProbeFactory,
            None,
            false,
        );
        let fake_probe = FakeProbe::with_mocked_core_and_binary(program_binary().as_path());
        fake_probe.expect_operation(Operation::ReadRawApRegister {
            ap: FullyQualifiedApAddress::v1_with_default_dp(1),
            address: 0xC,
            result: 1,
        });
        (probe_info, fake_probe)
    }

    async fn attach_once(client: &RpcClient, serial: &str) -> Result<()> {
        use crate::rpc::functions::probe::convert::from_wire_debug_probe_selector;
        use crate::util::cli::attach_probe;

        let probes = client.list_probes().await?;
        anyhow::ensure!(
            probes.iter().any(|p| p.serial_number == serial),
            "probe {serial} not listed"
        );

        let options = ProbeOptions {
            chip: Some(TEST_CHIP_NAME.to_owned()),
            chip_description_path: None,
            protocol: None,
            cycle_power: false,
            non_interactive: true,
            probe: probes
                .into_iter()
                .find(|p| p.serial_number == serial)
                .map(|p| from_wire_debug_probe_selector(p.selector())),
            speed: None,
            connect_under_reset: false,
            dry_run: false,
            allow_erase_all: false,
            attach_timeout: None,
        };

        attach_probe(client, options, None, false).await?;
        Ok(())
    }

    async fn with_test_lister_rpc(
        lister: Arc<TestLister>,
        f: impl AsyncFnOnce(&RpcClient) -> Result<()>,
    ) -> Result<()> {
        let probe_broker = Arc::new(crate::rpc::probe_broker::ProbeBroker::new());
        let (mut server, tx, rx) = RpcApp::create_server_with_lister(
            16,
            lister as Arc<dyn probe_rs::integration::ProbeLister + Send + Sync>,
            probe_broker,
        );
        let handle = tokio::spawn(async move {
            server.run().await;
        });
        let client = RpcClient::new_local_from_wire(tx, rx);
        let result = f(&client).await;
        drop(client);
        let _ = handle.await;
        result
    }

    #[tokio::test]
    async fn sequential_dap_rpc_connections_are_distinct() {
        let lister = Arc::new(TestLister::new());
        lister.probes.lock().unwrap().push(fake_probe("serial-a"));

        with_test_lister_rpc(lister.clone(), async |client| {
            attach_once(client, "serial-a").await
        })
        .await
        .unwrap();

        lister.probes.lock().unwrap().push(fake_probe("serial-b"));

        with_test_lister_rpc(lister, async |client| attach_once(client, "serial-b").await)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn per_connection_rpc_releases_session_before_next_client() {
        let lister = Arc::new(TestLister::new());

        with_test_lister_rpc(lister.clone(), async |client| {
            lister.probes.lock().unwrap().push(fake_probe("first"));
            attach_once(client, "first").await
        })
        .await
        .unwrap();

        with_test_lister_rpc(lister.clone(), async |client| {
            lister.probes.lock().unwrap().push(fake_probe("second"));
            attach_once(client, "second").await
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn with_dap_rpc_connection_clears_upload_cache_between_clients() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "payload").unwrap();
        let path = file.path().to_path_buf();

        let local_remote: RemoteParams = None;

        let first_hash = with_dap_rpc_connection(&local_remote, async |client| {
            let resolved = client.resolve_upload(&path).await?;
            Ok(resolved.content_hash)
        })
        .await
        .unwrap();

        let second_hash = with_dap_rpc_connection(&local_remote, async |client| {
            let resolved = client.resolve_upload(&path).await?;
            Ok(resolved.content_hash)
        })
        .await
        .unwrap();

        assert_eq!(first_hash, second_hash);
    }
}
