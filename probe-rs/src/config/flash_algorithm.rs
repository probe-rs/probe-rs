use super::flash_properties::FlashProperties;
use super::memory::{PageInfo, RamRegion, SectorInfo};
use crate::architecture::riscv;
use crate::core::Architecture;
use crate::flashing::FlashError;
use std::{borrow::Cow, convert::TryInto};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// Whether this flash algorithm is the default one or not.
    pub default: bool,
    /// Memory address where the flash algo instructions will be loaded to.
    pub load_address: u32,
    /// List of 32-bit words containing the position-independent code for the algo.
    pub instructions: Vec<u32>,
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u32>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u32>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u32,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u32,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u32>,
    /// Initial value of the R9 register for calling flash algo entry points, which
    /// determines where the position-independent data resides.
    pub static_base: u32,
    /// Initial value of the stack pointer when calling any flash algo API.
    pub begin_stack: u32,
    /// Base address of the page buffer. Used if `page_buffers` is not provided.
    pub begin_data: u32,
    /// An optional list of base addresses for page buffers. The buffers must be at
    /// least as large as the region's `page_size` attribute. If at least 2 buffers are included in
    /// the list, then double buffered programming will be enabled.
    pub page_buffers: Vec<u32>,

    /// The properties of the flash on the device.
    pub flash_properties: FlashProperties,
}

impl FlashAlgorithm {
    pub fn sector_info(&self, address: u32) -> Option<SectorInfo> {
        if !self.flash_properties.address_range.contains(&address) {
            log::trace!("Address {:08x} not contained in this flash device", address);
            return None;
        }

        let offset_address = address - self.flash_properties.address_range.start;

        let containing_sector = self
            .flash_properties
            .sectors
            .iter()
            .rfind(|s| s.address <= offset_address)?;

        let sector_index = (offset_address - containing_sector.address) / containing_sector.size;

        let sector_address = self.flash_properties.address_range.start
            + containing_sector.address
            + sector_index * containing_sector.size;

        Some(SectorInfo {
            base_address: sector_address,
            size: containing_sector.size,
            page_size: self.flash_properties.page_size,
        })
    }

    /// Returns the necessary information about the page which `address` resides in
    /// if the address is inside the flash region.
    pub fn page_info(&self, address: u32) -> Option<PageInfo> {
        if !self.flash_properties.address_range.contains(&address) {
            return None;
        }

        Some(PageInfo {
            base_address: address - (address % self.flash_properties.page_size),
            size: self.flash_properties.page_size,
        })
    }

    /// Returns true if the entire contents of the argument array equal the erased byte value.
    pub fn is_erased(&self, data: &[u8]) -> bool {
        for b in data {
            if *b != self.flash_properties.erased_byte_value {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: Cow<'static, str>,
    /// The description of the algorithm.
    pub description: Cow<'static, str>,
    /// Whether this flash algorithm is the default one or not.
    pub default: bool,
    /// List of 32-bit words containing the position-independent code for the algo.
    #[serde(deserialize_with = "deserialize")]
    #[serde(serialize_with = "serialize")]
    pub instructions: Cow<'static, [u8]>,
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u32>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u32>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u32,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u32,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u32>,
    /// The offset from the start of RAM to the data section.
    pub data_section_offset: u32,
    /// The properties of the flash on the device.
    pub flash_properties: FlashProperties,
}

pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&base64::encode(bytes))
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Cow<'static, [u8]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Base64Visitor;

    impl<'de> serde::de::Visitor<'de> for Base64Visitor {
        type Value = Cow<'static, [u8]>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(formatter, "base64 ASCII text")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            base64::decode(v)
                .map(Cow::Owned)
                .map_err(serde::de::Error::custom)
        }
    }

    deserializer.deserialize_str(Base64Visitor)
}

impl RawFlashAlgorithm {
    const FLASH_ALGO_STACK_SIZE: u32 = 512;
    const FLASH_ALGO_STACK_DECREMENT: u32 = 64;

    // Header for RISCV Flash Algorithms
    const RISCV_FLASH_BLOB_HEADER: [u32; 2] = [riscv::assembly::EBREAK, riscv::assembly::EBREAK];

    const ARM_FLASH_BLOB_HEADER: [u32; 8] = [
        0xE00A_BE00,
        0x062D_780D,
        0x2408_4068,
        0xD300_0040,
        0x1E64_4058,
        0x1C49_D1FA,
        0x2A00_1E52,
        0x0477_0D1F,
    ];

    fn get_algorithm_header(&self, architecture: Architecture) -> &[u32] {
        match architecture {
            Architecture::Arm => &Self::ARM_FLASH_BLOB_HEADER,
            Architecture::Riscv => &Self::RISCV_FLASH_BLOB_HEADER,
        }
    }

    /// Constructs a complete flash algorithm, tailored to the flash and RAM sizes given.
    pub fn assemble(
        &self,
        ram_region: &RamRegion,
        architecture: Architecture,
    ) -> Result<FlashAlgorithm, FlashError> {
        use std::mem::size_of;

        let assembled_instructions = self.instructions.chunks_exact(size_of::<u32>());

        if !assembled_instructions.remainder().is_empty() {
            return Err(FlashError::InvalidFlashAlgorithmLength);
        }

        let header = self.get_algorithm_header(architecture);
        let instructions: Vec<u32> = header
            .iter()
            .copied()
            .chain(
                assembled_instructions.map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())),
            )
            .collect();

        let mut offset = 0;
        let mut addr_stack = 0;
        let mut addr_load = 0;
        let mut addr_data = 0;

        // Try to find a stack size that fits with at least one page of data.
        for i in 0..Self::FLASH_ALGO_STACK_SIZE / Self::FLASH_ALGO_STACK_DECREMENT {
            offset = Self::FLASH_ALGO_STACK_SIZE - Self::FLASH_ALGO_STACK_DECREMENT * i;
            // Stack address
            addr_stack = ram_region.range.start + offset;
            // Load address
            addr_load = addr_stack;
            offset += (instructions.len() * size_of::<u32>()) as u32;

            // Data buffer 1
            addr_data = ram_region.range.start + offset;
            offset += self.flash_properties.page_size;

            if offset <= ram_region.range.end - ram_region.range.start {
                break;
            }
        }

        // Data buffer 2
        let addr_data2 = ram_region.range.start + offset;
        offset += self.flash_properties.page_size;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if offset <= ram_region.range.end - ram_region.range.start {
            vec![addr_data, addr_data2]
        } else {
            vec![addr_data]
        };

        let code_start = addr_load + (header.len() * size_of::<u32>()) as u32;

        let name = self.name.clone().into_owned();

        Ok(FlashAlgorithm {
            name,
            default: self.default,
            load_address: addr_load,
            instructions,
            pc_init: self.pc_init.map(|v| code_start + v),
            pc_uninit: self.pc_uninit.map(|v| code_start + v),
            pc_program_page: code_start + self.pc_program_page,
            pc_erase_sector: code_start + self.pc_erase_sector,
            pc_erase_all: self.pc_erase_all.map(|v| code_start + v),
            static_base: code_start + self.data_section_offset,
            begin_stack: addr_stack,
            begin_data: page_buffers[0],
            page_buffers: page_buffers.clone(),
            flash_properties: self.flash_properties.clone(),
        })
    }
}

#[test]
fn flash_sector_single_size() {
    use crate::config::SectorDescription;
    let config = FlashAlgorithm {
        flash_properties: FlashProperties {
            sectors: Cow::Borrowed(&[SectorDescription {
                size: 0x100,
                address: 0x0,
            }]),
            address_range: 0x1000..0x1000 + 0x1000,
            page_size: 0x10,
            ..Default::default()
        },
        ..Default::default()
    };

    let expected_first = SectorInfo {
        base_address: 0x1000,
        page_size: 0x10,
        size: 0x100,
    };

    assert!(config.sector_info(0x1000 - 1).is_none());

    assert_eq!(Some(expected_first), config.sector_info(0x1000));
    assert_eq!(Some(expected_first), config.sector_info(0x10ff));

    assert_eq!(Some(expected_first), config.sector_info(0x100b));
    assert_eq!(Some(expected_first), config.sector_info(0x10ea));
}

#[test]
fn flash_sector_single_size_weird_sector_size() {
    use crate::config::SectorDescription;
    let config = FlashAlgorithm {
        flash_properties: FlashProperties {
            sectors: Cow::Borrowed(&[SectorDescription {
                size: 258,
                address: 0x0,
            }]),
            address_range: 0x800_0000..0x800_0000 + 258 * 10,
            page_size: 0x10,
            ..Default::default()
        },
        ..Default::default()
    };

    let expected_first = SectorInfo {
        base_address: 0x800_0000,
        page_size: 0x10,
        size: 258,
    };

    assert!(config.sector_info(0x800_0000 - 1).is_none());

    assert_eq!(Some(expected_first), config.sector_info(0x800_0000));
    assert_eq!(Some(expected_first), config.sector_info(0x800_0000 + 257));

    assert_eq!(Some(expected_first), config.sector_info(0x800_000b));
    assert_eq!(Some(expected_first), config.sector_info(0x800_00e0));
}

#[test]
fn flash_sector_multiple_sizes() {
    use crate::config::SectorDescription;
    let config = FlashAlgorithm {
        flash_properties: FlashProperties {
            sectors: Cow::Borrowed(&[
                SectorDescription {
                    size: 0x4000,
                    address: 0x0,
                },
                SectorDescription {
                    size: 0x1_0000,
                    address: 0x1_0000,
                },
                SectorDescription {
                    size: 0x2_0000,
                    address: 0x2_0000,
                },
            ]),
            address_range: 0x800_0000..0x800_0000 + 0x10_0000,
            page_size: 0x10,
            ..Default::default()
        },
        ..Default::default()
    };

    let expected_a = SectorInfo {
        base_address: 0x800_4000,
        page_size: 0x10,
        size: 0x4000,
    };

    let expected_b = SectorInfo {
        base_address: 0x801_0000,
        page_size: 0x10,
        size: 0x1_0000,
    };

    let expected_c = SectorInfo {
        base_address: 0x80A_0000,
        page_size: 0x10,
        size: 0x2_0000,
    };

    assert_eq!(Some(expected_a), config.sector_info(0x800_4000));
    assert_eq!(Some(expected_b), config.sector_info(0x801_0000));
    assert_eq!(Some(expected_c), config.sector_info(0x80A_0000));
}
