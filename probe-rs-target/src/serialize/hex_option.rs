use super::serialize_u_int::SerializeUnsignedInt;
use serde::{Serialize, ser::Serializer};

pub fn serialize<T, S>(variant_value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize + std::fmt::LowerHex + SerializeUnsignedInt,
{
    match variant_value {
        Some(val) => {
            let check_for_human_readable = serializer.is_human_readable();
            if check_for_human_readable {
                serializer.serialize_some(format!("{val:#x}").as_str())
            } else {
                serializer.serialize_some(&val)
            }
        }
        None => serializer.serialize_none(),
    }
}
