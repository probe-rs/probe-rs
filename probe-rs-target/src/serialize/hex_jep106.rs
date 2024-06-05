use jep106::JEP106Code;
use serde::{Serialize, Serializer};

use crate::serialize::hex_u_int;

#[derive(Copy, Clone, PartialEq, Eq, Serialize)]
struct HexJep106 {
    #[serde(with = "hex_u_int")]
    id: u8,
    #[serde(with = "hex_u_int")]
    cc: u8,
}

impl From<JEP106Code> for HexJep106 {
    fn from(jep: JEP106Code) -> Self {
        Self {
            id: jep.id,
            cc: jep.cc,
        }
    }
}

pub fn serialize_option<S>(
    variant_value: &Option<JEP106Code>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match variant_value {
        Some(val) => {
            let val = HexJep106::from(*val);
            serializer.serialize_some(&val)
        }
        None => serializer.serialize_none(),
    }
}
