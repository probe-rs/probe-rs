use serde::{Deserialize, Serialize};

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct Request {
    pub(crate) Values: Vec<Pack>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct Pack {
    pub(crate) Id: String,
    pub(crate) Name: String,
    pub(crate) ShortName: String,
    pub(crate) Vendor: String,
    pub(crate) Description: String,
    pub(crate) IsDevicePack: bool,
    pub(crate) IsBoardPack: bool,
    pub(crate) IsThirdParty: bool,
    pub(crate) PackUrl: String,
}

pub(crate) fn list_packs() -> Result<Vec<Pack>, reqwest::Error> {
    let packs = reqwest::blocking::Client::new()
        .get("https://api.arm.com/e-cmsis/v3/packs")
        .header("uuid", "97822e53-79f4-4ca0-a6cb-c8e0acb57282")
        .send()?
        .json::<Request>()?
        .Values;

    return Ok(packs);
}
