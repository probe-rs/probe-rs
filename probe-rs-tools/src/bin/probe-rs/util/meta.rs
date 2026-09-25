use anyhow::{Context, Result};
use probe_rs_mi::meta::Meta;

#[cfg(feature = "remote")]
#[cfg(feature = "remote")]
pub fn rpc_user_agent() -> String {
    format!("probe-rs-tools {}", env!("PROBE_RS_LONG_VERSION"))
}

pub fn current_meta() -> Result<Meta> {
    Ok(Meta {
        version: env!("PROBE_RS_VERSION")
            .parse()
            .context("failed to parse the built in version info")?,
        commit: env!("PROBE_RS_LONG_VERSION"),
        arch: std::env::consts::ARCH,
        os: std::env::consts::OS,
    })
}
