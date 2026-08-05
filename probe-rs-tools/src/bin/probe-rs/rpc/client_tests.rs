use std::io::Write;

use probe_rs_rpc_client::RpcClient;
use tempfile::NamedTempFile;

use std::sync::Arc;

use crate::rpc::functions::{ProbeAccess, RpcApp};

#[tokio::test]
async fn local_resolve_upload_tracks_content_changes() {
    let (_server, tx, rx) = RpcApp::create_server(
        16,
        ProbeAccess::All,
        Arc::new(crate::rpc::probe_broker::ProbeBroker::new()),
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
