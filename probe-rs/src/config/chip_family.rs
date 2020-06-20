use super::chip::Chip;
use super::flash_algorithm::RawFlashAlgorithm;
use crate::config::TargetParseError;
use jep106::JEP106Code;
use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// This describes a chip family with all its variants.
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
}

pub fn serialize<S>(raw_algorithms: &[RawFlashAlgorithm], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use crate::serde::ser::SerializeMap;
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
    pub fn from_yaml_reader<R: std::io::Read>(
        definition_reader: R,
    ) -> Result<Self, TargetParseError> {
        serde_yaml::from_reader(definition_reader)
    }

    pub fn variants(&self) -> &[Chip] {
        &self.variants
    }

    pub fn algorithms(&self) -> &[RawFlashAlgorithm] {
        &self.flash_algorithms
    }

    pub fn get_algorithm(&self, name: impl AsRef<str>) -> Option<&RawFlashAlgorithm> {
        let name = name.as_ref();
        self.flash_algorithms.iter().find(|elem| elem.name == name)
    }
}

#[test]
fn map_to_list_deserialize() {
    let result: Result<ChipFamily, _> =
        serde_yaml::from_str(include_str!("../../targets/STM32F4 Series.yaml"));
    assert!(result.is_ok());

    let chip_family = result.unwrap();
    assert_eq!(chip_family.algorithms().len(), 18);
}
