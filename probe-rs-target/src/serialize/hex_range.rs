use serde::{self, ser::SerializeStruct, Serializer};
use std::ops::Range;

pub fn serialize<S>(memory_range: &Range<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    // We serialize the range as hex strings when generating human-readable formats such as YAML,
    let check_for_human_readable = serializer.is_human_readable();
    let mut state = serializer.serialize_struct("Range", 2)?;
    if check_for_human_readable {
        state.serialize_field("start", format!("{:#x}", memory_range.start).as_str())?;
        state.serialize_field("end", format!("{:#x}", memory_range.end).as_str())?;
    } else {
        state.serialize_field("start", &memory_range.start)?;
        state.serialize_field("end", &memory_range.end)?;
    }
    state.end()
}
