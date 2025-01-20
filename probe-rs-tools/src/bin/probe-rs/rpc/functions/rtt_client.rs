use std::path::PathBuf;

use crate::{
    rpc::{
        functions::{RpcContext, RpcResult},
        Key,
    },
    util::rtt::{client::RttClient, RttChannelConfig, RttConfig},
};
use anyhow::Context as _;
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{flashing::FormatKind, rtt::ScanRegion, Session};
use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Serialize, Deserialize, Schema)]
pub struct LogOptions {
    /// Suppress filename and line number information from the rtt log
    pub no_location: bool,

    /// The format string to use when printing defmt encoded log messages from the target.
    ///
    /// See https://defmt.ferrous-systems.com/custom-log-output
    pub log_format: Option<String>,

    /// Scan the memory to find the RTT control block
    pub rtt_scan_memory: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct CreateRttClientRequest {
    pub sessid: Key<Session>,
    pub path: Option<PathBuf>,
    pub log_options: LogOptions,
}

pub type CreateRttClientResponse = RpcResult<Key<RttClient>>;

pub async fn create_rtt_client(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CreateRttClientRequest,
) -> CreateRttClientResponse {
    let session = ctx.session(request.sessid).await;

    let rtt_scan_regions = match request.log_options.rtt_scan_memory {
        true => session.target().rtt_scan_regions.clone(),
        false => ScanRegion::Ranges(vec![]),
    };
    let mut rtt_config = RttConfig::default();
    rtt_config.channels.push(RttChannelConfig {
        channel_number: Some(0),
        show_location: !request.log_options.no_location,
        log_format: request.log_options.log_format.clone(),
        ..Default::default()
    });
    let elf = if let Some(path) = request.path.as_deref() {
        let format = FormatKind::from_optional(session.target().default_format.as_deref())
            .expect("Failed to parse a default binary format. This shouldn't happen.");
        if matches!(format, FormatKind::Elf | FormatKind::Idf) {
            Some(
                tokio::fs::read(path)
                    .await
                    .context("Failed to open firmware binary")?,
            )
        } else {
            None
        }
    } else {
        None
    };

    let client = RttClient::new(
        elf.as_deref(),
        session.target(),
        rtt_config,
        rtt_scan_regions,
    )?;

    Ok(ctx.store_object(client).await)
}
