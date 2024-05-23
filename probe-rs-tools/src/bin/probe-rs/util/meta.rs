use anyhow::{Context, Result};
use probe_rs_mi::meta::Meta;

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
