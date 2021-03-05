use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use jep106::JEP106Code;
use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Source of a target description.
///
/// This is used for diagnostics, when
/// an error related to a target description occurs.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetDescriptionSource {
    /// The target description is a generic target description,
    /// which just describes a core type (e.g. M4), without any
    /// flash algorithm or memory description.
    Generic,
    /// The target description is a built-in target description,
    /// which was included into probe-rs at compile time.
    BuiltIn,
    /// The target description was from an external source
    /// during runtime.
    External,
}

/// This describes a chip family with all its variants.
///
/// This struct is usually read from a target description
/// file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipFamily {
    /// This is the name of the chip family in base form.
    /// E.g. `nRF52832`.
    pub name: Cow<'static, str>,
    /// The JEP106 code of the manufacturer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<JEP106Code>,
    /// This vector holds all the variants of the family.
    pub variants: Cow<'static, [Chip]>,
    /// This vector holds all available algorithms.
    #[serde(deserialize_with = "deserialize")]
    #[serde(serialize_with = "serialize")]
    pub flash_algorithms: Cow<'static, [RawFlashAlgorithm]>,
    /// The name of the core type.
    /// E.g. `M0` or `M4`.
    pub core: Cow<'static, str>,

    #[serde(skip, default = "default_source")]
    /// Source of the target description, used for diagnostics
    pub source: TargetDescriptionSource,
}

// When deserialization is used, this means that the target is read from an external source.
fn default_source() -> TargetDescriptionSource {
    TargetDescriptionSource::External
}

pub fn serialize<S>(raw_algorithms: &[RawFlashAlgorithm], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(raw_algorithms.len()))?;
    for entry in raw_algorithms {
        map.serialize_entry(entry.name.as_ref(), entry)?;
    }
    map.end()
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Cow<'static, [RawFlashAlgorithm]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct MapVisitor;

    use serde::de::MapAccess;
    impl<'de> serde::de::Visitor<'de> for MapVisitor {
        type Value = Cow<'static, [RawFlashAlgorithm]>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(formatter, "a map")
        }

        fn visit_map<A>(self, mut v: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut result = vec![];
            while let Some((_key, value)) = v.next_entry::<String, RawFlashAlgorithm>()? {
                result.push(value);
            }

            Ok(Cow::Owned(result))
        }
    }

    deserializer.deserialize_map(MapVisitor)
}

impl ChipFamily {
    /// Get the different [Chip]s which are part of this
    /// family.
    pub fn variants(&self) -> &[Chip] {
        &self.variants
    }

    /// Get all flash algorithms for this family of chips.
    pub fn algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.flash_algorithms
    }

    /// Try to find a [RawFlashAlgorithm] with a given name.
    pub fn get_algorithm(&self, name: impl AsRef<str>) -> Option<&RawFlashAlgorithm> {
        let name = name.as_ref();
        self.flash_algorithms.iter().find(|elem| elem.name == name)
    }
}
