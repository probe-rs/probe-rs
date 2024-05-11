use anyhow::Context;
use probe_rs_mi::meta::Meta;

pub fn run() -> anyhow::Result<()> {
    let meta = Meta {
        version: env!("PROBE_RS_VERSION")
            .parse()
            .context("failed to parse the built in version info")?,
        commit: env!("GIT_REV"),
        arch: std::env::consts::ARCH,
        os: std::env::consts::OS,
    };
    let meta = serde_json::to_string(&meta)?;
    println!("{meta}");
    Ok(())
}
