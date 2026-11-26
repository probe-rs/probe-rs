//! # Extract metadata from an object file
//!
//! An image can say how probe-rs should run it, for example which chip it is built for. The
//! known keys are in [`keys`]. Read one with [`ElfMetadata::get`].

use std::collections::HashMap;
use std::marker::PhantomData;
use std::ptr;

use object::{Object, ObjectSection, ObjectSymbol};

use crate::Target;
use crate::config::{Registry, RegistryError};

/// Where an image stores a metadata value.
#[derive(Debug, Clone, Copy)]
pub enum Location {
    /// A `.probe-rs.*` section that the `probe_rs_meta` crate writes. The section holds the
    /// value in binary: UTF-8 text or a little-endian number.
    ProbeRs(&'static str),

    /// A symbol in the `.espressif.metadata` section. The symbol holds the value as text.
    Espressif(&'static str),
}

/// A metadata key: the places that can hold it, in order of priority, and the type of its value.
pub struct Key<T> {
    locations: &'static [Location],
    value: PhantomData<fn() -> T>,
}

impl<T> Key<T> {
    /// A key that is read from the first of `locations` that the image has.
    pub const fn new(locations: &'static [Location]) -> Self {
        Self {
            locations,
            value: PhantomData,
        }
    }
}

/// The known metadata keys.
pub mod keys {
    use super::{Key, Location};

    /// The version of the `probe_rs_meta` crate that the image is built with.
    pub const PROBE_RS_META_VERSION: Key<String> =
        Key::new(&[Location::ProbeRs(".probe-rs.version")]);

    /// The chip that the image is built for.
    pub const CHIP: Key<String> = Key::new(&[
        Location::ProbeRs(".probe-rs.chip"),
        Location::Espressif("build_info.CHIP_NAME"),
    ]);

    /// The time in seconds that the program may run until it hits a breakpoint or a fault.
    pub const TIMEOUT: Key<u32> = Key::new(&[Location::ProbeRs(".probe-rs.timeout")]);

    /// The minimum chip revision that the image runs on, as `major * 100 + minor`.
    pub const MIN_CHIP_REVISION: Key<u16> =
        Key::new(&[Location::Espressif("build_info.MIN_CHIP_REVISION")]);

    /// The format of the image's log output.
    pub const LOG_FORMAT: Key<String> = Key::new(&[Location::Espressif("espflash.LOG_FORMAT")]);

    /// The bootloader that starts the image, for example `ESP-IDF` or `DIRECT-BOOT`.
    pub const BOOTLOADER_NAME: Key<String> = Key::new(&[Location::Espressif("bootloader.NAME")]);
}

/// A type that a metadata value can have.
pub trait MetadataValue: Sized {
    /// Decodes the binary value of a `.probe-rs.*` section.
    fn from_binary(data: &[u8]) -> Option<Self>;

    /// Decodes a value that the image stores as text.
    fn from_text(text: &str) -> Option<Self>;
}

impl MetadataValue for String {
    fn from_binary(data: &[u8]) -> Option<Self> {
        String::from_utf8(data.to_vec()).ok()
    }

    fn from_text(text: &str) -> Option<Self> {
        Some(text.to_string())
    }
}

macro_rules! integer_metadata_value {
    ($($ty:ty),*) => {
        $(
            impl MetadataValue for $ty {
                fn from_binary(data: &[u8]) -> Option<Self> {
                    data.try_into().ok().map(<$ty>::from_le_bytes)
                }

                fn from_text(text: &str) -> Option<Self> {
                    text.parse().ok()
                }
            }
        )*
    };
}

integer_metadata_value!(u16, u32);

/// The metadata of an image: the contents of its `.probe-rs.*` sections and of the symbols in its
/// `.espressif.metadata` section.
#[derive(Clone, Debug, Default)]
pub struct ElfMetadata {
    /// The `.probe-rs.*` sections, by section name.
    probe_rs: HashMap<String, Vec<u8>>,

    /// The symbols in `.espressif.metadata`, by symbol name.
    espressif: HashMap<String, Vec<u8>>,
}

impl ElfMetadata {
    /// Construct probe-rs-meta information from unparsed bytes
    pub fn from_elf(elf: &[u8]) -> Result<Self, object::Error> {
        let file = object::File::parse(elf)?;

        Self::from_object(&file)
    }

    /// Construct probe-rs-meta information from a parsed object
    pub fn from_object(file: &object::File) -> Result<Self, object::Error> {
        let mut metadata = Self::default();

        for section in file.sections() {
            if let Ok(name) = section.name()
                && name.starts_with(".probe-rs.")
            {
                let data = section.data()?;
                metadata.probe_rs.insert(name.to_string(), data.to_vec());
            }
        }

        if let Some(section) = file.section_by_name(".espressif.metadata") {
            for symbol in file.symbols() {
                if symbol.section_index() != Some(section.index()) {
                    continue;
                }
                let name = symbol.name()?;
                if let Some(data) = section.data_range(symbol.address(), symbol.size())? {
                    metadata.espressif.insert(name.to_string(), data.to_vec());
                }
            }
        }

        Ok(metadata)
    }

    /// The value of `key` from the first of its locations that the image has. Returns `None` if
    /// the image has no value for `key`, or, with a warning, if the value is not valid.
    pub fn get<T: MetadataValue>(&self, key: &Key<T>) -> Option<T> {
        for location in key.locations {
            let value = match *location {
                Location::ProbeRs(section) => {
                    let Some(data) = self.probe_rs.get(section) else {
                        continue;
                    };
                    T::from_binary(data)
                }
                Location::Espressif(symbol) => {
                    let Some(data) = self.espressif.get(symbol) else {
                        continue;
                    };
                    std::str::from_utf8(data).ok().and_then(T::from_text)
                }
            };

            if value.is_none() {
                tracing::warn!("The image metadata {location:?} is not valid.");
            }
            return value;
        }

        None
    }
}

/// The chip that an image is built for, as the registry knows it.
#[derive(Debug)]
pub struct ImageChip {
    /// The name in the image's metadata.
    pub name: String,
}

impl ImageChip {
    /// Reads the chip that `image` is built for. Returns `None` if the image names no chip that
    /// the registry can use. An image that is not an ELF file has no metadata.
    pub fn from_image(image: &[u8], registry: &Registry) -> Option<Self> {
        let metadata = ElfMetadata::from_elf(image).ok()?;
        Self::from_metadata(&metadata, registry)
    }

    /// Reads the chip that an image with `metadata` is built for. Returns `None` if the metadata
    /// names no chip that the registry can use.
    pub fn from_metadata(metadata: &ElfMetadata, registry: &Registry) -> Option<Self> {
        let name = metadata.get(&keys::CHIP)?;
        Self::resolve(&name, registry)
    }

    /// Resolves `name` under the `--chip` rules. Returns `None` if the registry cannot use it.
    pub fn resolve(name: &str, registry: &Registry) -> Option<Self> {
        if let Err(error) = registry.get_family_and_chip_by_name(name) {
            tracing::warn!("The image is built for {name}, which is not usable: {error}");
            return None;
        }

        Some(Self {
            name: name.to_string(),
        })
    }

    /// Fails if the image is built for a different chip than `target`.
    pub fn check_target(&self, target: &Target, registry: &Registry) -> Result<(), ImageChipError> {
        // The variant name and its package names select the same chip, so compare the variants
        // that the names select.
        let (image_family, image_variant) = registry.get_family_and_chip_by_name(&self.name)?;
        let (target_family, target_variant) = registry.get_family_and_chip_by_name(&target.name)?;

        if ptr::eq(image_variant, target_variant) {
            return Ok(());
        }

        // Detection by part number finds any variant of the family with that part, or with no
        // part: `RP235x` for `RP235x_riscv`, `STM32WBA65RI` for `STM32WBA55CGUx`.
        let same_part = ptr::eq(image_family, target_family)
            && target_variant.part.is_some()
            && (image_variant.part.is_none() || image_variant.part == target_variant.part);

        let target = target.name.clone();
        let image_chip = self.name.clone();
        let error = if same_part {
            ImageChipError::VariantMismatch { target, image_chip }
        } else {
            ImageChipError::Mismatch { target, image_chip }
        };
        Err(error)
    }
}

/// The image is built for a different chip than the target.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum ImageChipError {
    /// The image is built for {image_chip}, but the target is {target}.
    Mismatch {
        /// The target chip.
        target: String,
        /// The chip that the image is built for.
        image_chip: String,
    },

    /// The image is built for {image_chip}, but the target is {target}. Use `--chip {image_chip}` if the image is right for this chip.
    VariantMismatch {
        /// The target chip.
        target: String,
        /// The chip that the image is built for.
        image_chip: String,
    },

    /// The registry does not list the chip.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn probe_rs_metadata(section: &str, data: &[u8]) -> ElfMetadata {
        ElfMetadata {
            probe_rs: HashMap::from([(section.to_string(), data.to_vec())]),
            ..Default::default()
        }
    }

    #[test]
    fn probe_rs_value_is_read_as_binary() {
        let metadata = probe_rs_metadata(".probe-rs.timeout", &60u32.to_le_bytes());

        assert_eq!(metadata.get(&keys::TIMEOUT), Some(60));
    }

    #[test]
    fn value_of_the_wrong_size_is_invalid() {
        let metadata = probe_rs_metadata(".probe-rs.timeout", &[60, 0]);

        assert_eq!(metadata.get(&keys::TIMEOUT), None);
    }

    #[test]
    fn probe_rs_chip_comes_before_espressif_chip() {
        let metadata = ElfMetadata {
            espressif: HashMap::from([("build_info.CHIP_NAME".to_string(), b"esp32c6".to_vec())]),
            ..probe_rs_metadata(".probe-rs.chip", b"esp32c3")
        };

        assert_eq!(metadata.get(&keys::CHIP), Some("esp32c3".to_string()));
    }

    #[test]
    fn espressif_metadata_is_read_as_text() {
        let image = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../probe-rs-debug/tests/debug-unwind-tests/async_esp32c6.elf"),
        )
        .unwrap();

        let metadata = ElfMetadata::from_elf(&image).unwrap();

        assert_eq!(metadata.get(&keys::CHIP), Some("esp32c6".to_string()));
        assert!(metadata.get(&keys::MIN_CHIP_REVISION).is_some());
        assert_eq!(
            metadata.get(&keys::BOOTLOADER_NAME),
            Some("ESP-IDF".to_string())
        );
    }

    #[cfg(feature = "builtin-targets")]
    mod image_chip {
        use super::*;
        use std::assert_matches;

        /// Checks an image built for `image_chip` against the target `target_name`.
        fn check(image_chip: &str, target_name: &str) -> Result<(), ImageChipError> {
            let registry = Registry::from_builtin_families();
            let target = registry.get_target_by_name(target_name).unwrap();

            ImageChip::resolve(image_chip, &registry)
                .unwrap()
                .check_target(&target, &registry)
        }

        #[test]
        fn package_of_the_target_variant_passes_the_check() {
            assert!(check("STM32G030C6Tx", "STM32G030C6").is_ok());
        }

        #[test]
        fn image_for_another_variant_fails_the_check() {
            let result = check("STM32G030C8Tx", "STM32G030C6Tx");

            assert_matches!(
                result,
                Err(ImageChipError::Mismatch { target, image_chip })
                    if target == "STM32G030C6Tx" && image_chip == "STM32G030C8Tx"
            );
        }

        #[test]
        fn variant_with_the_image_part_suggests_chip_option() {
            let result = check("RP235x_riscv", "RP235x");

            assert_matches!(
                result,
                Err(ImageChipError::VariantMismatch { target, image_chip })
                    if target == "RP235x" && image_chip == "RP235x_riscv"
            );
        }

        #[test]
        fn variant_found_by_part_suggests_chip_option_for_an_image_variant_without_part() {
            let result = check("STM32WBA55CGUx", "STM32WBA65RI");

            assert_matches!(result, Err(ImageChipError::VariantMismatch { .. }));
        }

        #[test]
        fn chip_of_another_family_fails_without_suggestion() {
            let result = check("RP235x_riscv", "STM32G030C6Tx");

            assert_matches!(result, Err(ImageChipError::Mismatch { .. }));
        }

        #[test]
        fn variant_without_part_fails_without_suggestion() {
            // Nordic detection reads the exact variant from the FICR, not from a part number.
            let result = check("nRF52832_xxAA", "nRF52840_xxAA");

            assert_matches!(result, Err(ImageChipError::Mismatch { .. }));
        }
    }
}
