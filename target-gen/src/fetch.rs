use anyhow::Result;
use cmsis_pack::{pack_index::Vidx, utils::FromElem};

/// Fetches the master VIDX/PIDX file from the ARM server and returns the parsed file.
pub async fn get_vidx() -> Result<Vidx> {
    let reader = reqwest::Client::new()
        .get("https://www.keil.com/pack/index.pidx")
        .send()
        .await?
        .text()
        .await?;

    let vidx = Vidx::from_string(&reader)?;

    Ok(vidx)
}
