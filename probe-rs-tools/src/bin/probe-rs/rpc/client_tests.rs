use std::io::Write;

use probe_rs_rpc_client::{ClientError, RpcClient, SemihostingOptions};
use tempfile::NamedTempFile;

use std::sync::Arc;

use crate::rpc::functions::{HostFiles, LoginSecrets, ProbeAccess, RpcApp};
use crate::rpc::probe_broker::ProbeBroker;

#[tokio::test]
async fn local_resolve_upload_tracks_content_changes() {
    let (_server, tx, rx) = RpcApp::create_server(
        16,
        ProbeAccess::All,
        Arc::new(ProbeBroker::new()),
        HostFiles::Allow,
    );
    let client = RpcClient::new_local_from_wire(tx, rx);

    let mut file = NamedTempFile::new().unwrap();
    write!(file, "v1").unwrap();
    let path = file.path().to_path_buf();

    let first = client.resolve_upload(&path).await.unwrap();
    write!(file, "v2").unwrap();
    let second = client.resolve_upload(&path).await.unwrap();

    assert_ne!(first.content_hash, second.content_hash);
    assert_eq!(first.remote_path, second.remote_path);
}

#[tokio::test]
async fn local_server_schema_matches_the_client() {
    let (server, tx, rx) = RpcApp::create_server(
        16,
        ProbeAccess::All,
        Arc::new(ProbeBroker::new()),
        HostFiles::Allow,
    );
    let handle = tokio::spawn(async move { server.run().await });
    let client = RpcClient::new_local_from_wire(tx, rx);

    client
        .check_compatibility()
        .await
        .expect("the in-process server must match the client schema");

    drop(client);
    let _ = handle.await;
}

/// Runs a server for a login with the token `token`, and returns a client of it.
fn client_after_login_from(peer_is_loopback: bool) -> RpcClient {
    let (server, tx, rx) = RpcApp::create_server_after_login(
        16,
        ProbeAccess::All,
        Arc::new(ProbeBroker::new()),
        LoginSecrets {
            challenge: "challenge".to_owned(),
            token: "token".to_owned(),
            peer_is_loopback,
        },
    );
    tokio::spawn(server.run());
    RpcClient::new_from_wire(tx, rx)
}

/// Like [`client_after_login_from`], for a login through `ssh -W`.
fn client_after_login() -> RpcClient {
    client_after_login_from(true)
}

#[tokio::test]
async fn only_a_server_that_knows_the_token_is_trusted() {
    let unverified = client_after_login();
    assert!(!unverified.server_is_trusted());

    let verified = client_after_login()
        .with_server_proof("challenge", Some("token"))
        .await;
    assert!(verified.server_is_trusted());

    let wrong_token = client_after_login()
        .with_server_proof("challenge", Some("other"))
        .await;
    assert!(!wrong_token.server_is_trusted());

    let wrong_challenge = client_after_login()
        .with_server_proof("other", Some("token"))
        .await;
    assert!(!wrong_challenge.server_is_trusted());

    let no_token = client_after_login()
        .with_server_proof("challenge", None)
        .await;
    assert!(!no_token.server_is_trusted());
}

/// Another local user of the ssh host can relay the connection to another server that has the
/// same token. That server sees a client from another host, so its proof does not match.
#[tokio::test]
async fn a_relayed_server_is_not_trusted() {
    let relayed = client_after_login_from(false)
        .with_server_proof("challenge", Some("token"))
        .await;
    assert!(!relayed.server_is_trusted());
}

#[tokio::test]
async fn a_local_server_is_trusted_and_has_no_proof() {
    let (server, tx, rx) = RpcApp::create_server(
        16,
        ProbeAccess::All,
        Arc::new(ProbeBroker::new()),
        HostFiles::Allow,
    );
    tokio::spawn(server.run());
    let client = RpcClient::new_local_from_wire(tx, rx);
    assert!(client.server_is_trusted());

    let (server, tx, rx) = RpcApp::create_server(
        16,
        ProbeAccess::All,
        Arc::new(ProbeBroker::new()),
        HostFiles::Deny,
    );
    tokio::spawn(server.run());
    let client = RpcClient::new_from_wire(tx, rx)
        .with_server_proof("challenge", Some("token"))
        .await;
    assert!(!client.server_is_trusted());
}

#[tokio::test]
async fn host_files_need_a_trusted_server_or_an_opt_in() {
    let mut options = SemihostingOptions::new();
    options
        .add_file("in.txt".to_owned(), "host.txt".to_owned())
        .unwrap();
    let host_files = probe_rs_rpc_client::HostFiles::new(&options).unwrap();

    let unverified = client_after_login();
    assert!(matches!(
        unverified.check_host_files(&host_files),
        Err(ClientError::UnverifiedServer)
    ));
    assert!(
        unverified
            .check_host_files(&host_files.clone().allow_unverified_server(true))
            .is_ok()
    );

    let verified = client_after_login()
        .with_server_proof("challenge", Some("token"))
        .await;
    assert!(verified.check_host_files(&host_files).is_ok());
}
