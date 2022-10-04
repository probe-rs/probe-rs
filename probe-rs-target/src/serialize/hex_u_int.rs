use super::serialize_u_int::SerializeUnsignedInt;
use serde::{self, Serializer};

pub(crate) fn serialize<T, S>(memory_address: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: std::fmt::LowerHex + SerializeUnsignedInt,
{
    // We serialize the range as hex strings when generating human-readable formats such as YAML,
    let check_for_human_readable = serializer.is_human_readable();
    if check_for_human_readable {
        serializer.serialize_str(format!("{:#x}", memory_address).as_str())
    } else {
        memory_address.serialize_int(serializer)
    }
}
