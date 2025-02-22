use indexmap::IndexMap;
use serde::{Serialize, Serializer, ser::SerializeMap as _};

use crate::serialize::{hex_u_int, serialize_u_int::SerializeUnsignedInt};

#[derive(Serialize)]
struct Hex<I: Serialize + std::fmt::LowerHex + SerializeUnsignedInt>(
    #[serde(serialize_with = "hex_u_int")] I,
);

pub fn serialize<I, T, S>(map: &IndexMap<I, T>, serializer: S) -> Result<S::Ok, S::Error>
where
    I: Serialize + std::fmt::LowerHex + SerializeUnsignedInt + Copy,
    T: Serialize,
    S: Serializer,
{
    let mut map_ser = serializer.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        map_ser.serialize_entry(&Hex(*k), v)?;
    }
    map_ser.end()
}
