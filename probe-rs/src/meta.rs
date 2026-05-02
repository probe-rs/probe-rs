//! # Extract metadata from an object file

use tracing::warn;

use object::Object;
use object::ObjectSection;

/// Contains probe-rs-meta information
#[derive(Clone, Debug)]
pub struct ElfMetadata {
    /// `probe_rs_meta::chip`
    pub chip: Option<String>,
    /// `probe_rs_meta::timeout`
    pub timeout: Option<u64>,
}

impl ElfMetadata {
    /// Construct probe-rs-meta information from unparsed bytes
    pub fn from_elf(elf: &[u8]) -> Result<Self, object::Error> {
        let file = object::File::parse(elf)?;

        Self::from_object(&file)
    }

    /// Construct probe-rs-meta information from a parsed object
    pub fn from_object(file: &object::File) -> Result<Self, object::Error> {
        let mut chip = None;
        let mut timeout = None;

        if let Some(section) = file.section_by_name(".probe-rs.chip") {
            let data = section.data()?;
            if !data.is_empty() {
                match String::from_utf8(data.to_vec()) {
                    Ok(s) => chip = Some(s),
                    Err(_) => warn!(".probe-rs.chip contents are not a valid utf8 string."),
                }
            }
        }

        if let Some(section) = file.section_by_name(".teleprobe.timeout") {
            let data = section.data()?;
            if data.len() == 4 {
                timeout = Some(u32::from_le_bytes(data.try_into().unwrap()) as u64)
            } else {
                warn!(".probe-rs.timeout contents are not a valid u32.")
            }
        }

        Ok(Self { chip, timeout })
    }
}
