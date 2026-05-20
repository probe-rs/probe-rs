use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serialize a `IndexMap<String, u64>` with values as hex strings in human-readable formats
/// (e.g. YAML), and as plain integers in binary formats.
///
/// `IndexMap` is used instead of `HashMap` to guarantee deterministic key order in the output.
pub(crate) fn serialize<S>(map: &IndexMap<String, u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if serializer.is_human_readable() {
        use serde::ser::SerializeMap;
        let mut s = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            s.serialize_entry(k, &format!("{v:#x}"))?;
        }
        s.end()
    } else {
        map.serialize(serializer)
    }
}

/// Deserialize a `IndexMap<String, u64>`. In YAML, values may be written as hex literals
/// (`0x1234`) or plain decimal integers; `yaml_serde` handles both natively.
pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<IndexMap<String, u64>, D::Error>
where
    D: Deserializer<'de>,
{
    IndexMap::<String, u64>::deserialize(deserializer)
}
