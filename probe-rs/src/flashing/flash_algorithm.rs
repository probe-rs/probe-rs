use probe_rs_target::{FlashProperties, PageInfo, RamRegion, RawFlashAlgorithm, SectorInfo};

use super::FlashError;
use crate::core::Architecture;
use crate::{architecture::riscv, Target};
use std::convert::TryInto;

/// A flash algorithm, which has been assembled for a specific
/// chip.
///
/// To create a [FlashAlgorithm], call the [`assemble_from_raw`] function.
///
/// [`assemble_from_raw`]: FlashAlgorithm::assemble_from_raw
#[derive(Debug, Default, Clone)]
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
    /// Try to retrieve the information about the flash sector
    /// which contains `address`.
    ///
    /// If the `address` is not part of the flash, None will
    /// be returned.
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

    /// Iterate over all the sectors of the flash.
    pub fn iter_sectors(&self) -> impl Iterator<Item = SectorInfo> + '_ {
        let props = &self.flash_properties;

        assert!(!props.sectors.is_empty());
        assert!(props.sectors[0].address == 0);

        let mut addr = props.address_range.start;
        let mut desc_idx = 0;
        std::iter::from_fn(move || {
            if addr >= props.address_range.end {
                return None;
            }

            // Advance desc_idx if needed
            if let Some(next_desc) = props.sectors.get(desc_idx + 1) {
                if props.address_range.start + next_desc.address <= addr {
                    desc_idx += 1;
                }
            }

            let size = props.sectors[desc_idx].size;
            let sector = SectorInfo {
                base_address: addr,
                size,
            };
            addr += size;

            Some(sector)
        })
    }

    /// Iterate over all the pages of the flash.
    pub fn iter_pages(&self) -> impl Iterator<Item = PageInfo> + '_ {
        let props = &self.flash_properties;

        let mut addr = props.address_range.start;
        std::iter::from_fn(move || {
            if addr >= props.address_range.end {
                return None;
            }

            let page = PageInfo {
                base_address: addr,
                size: props.page_size,
            };
            addr += props.page_size;

            Some(page)
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

    fn get_algorithm_header(architecture: Architecture) -> &'static [u32] {
        match architecture {
            Architecture::Arm => &Self::ARM_FLASH_BLOB_HEADER,
            Architecture::Riscv => &Self::RISCV_FLASH_BLOB_HEADER,
        }
    }

    /// Constructs a complete flash algorithm, tailored to the flash and RAM sizes given.
    pub fn assemble_from_raw(
        raw: &RawFlashAlgorithm,
        ram_region: &RamRegion,
        target: &Target,
    ) -> Result<Self, FlashError> {
        use std::mem::size_of;

        let assembled_instructions = raw.instructions.chunks_exact(size_of::<u32>());

        if !assembled_instructions.remainder().is_empty() {
            return Err(FlashError::InvalidFlashAlgorithmLength {
                name: raw.name.to_string(),
                algorithm_source: Some(target.source().clone()),
            });
        }

        let header = Self::get_algorithm_header(target.architecture());
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
            offset += raw.flash_properties.page_size;

            if offset <= ram_region.range.end - ram_region.range.start {
                break;
            }
        }

        // Data buffer 2
        let addr_data2 = ram_region.range.start + offset;
        offset += raw.flash_properties.page_size;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if offset <= ram_region.range.end - ram_region.range.start {
            vec![addr_data, addr_data2]
        } else {
            vec![addr_data]
        };

        let code_start = addr_load + (header.len() * size_of::<u32>()) as u32;

        let name = raw.name.clone();

        Ok(FlashAlgorithm {
            name,
            default: raw.default,
            load_address: addr_load,
            instructions,
            pc_init: raw.pc_init.map(|v| code_start + v),
            pc_uninit: raw.pc_uninit.map(|v| code_start + v),
            pc_program_page: code_start + raw.pc_program_page,
            pc_erase_sector: code_start + raw.pc_erase_sector,
            pc_erase_all: raw.pc_erase_all.map(|v| code_start + v),
            static_base: code_start + raw.data_section_offset,
            begin_stack: addr_stack,
            begin_data: page_buffers[0],
            page_buffers: page_buffers.clone(),
            flash_properties: raw.flash_properties.clone(),
        })
    }
}

#[cfg(test)]
mod test {
    use probe_rs_target::{FlashProperties, SectorDescription, SectorInfo};

    use crate::flashing::FlashAlgorithm;

    #[test]
    fn flash_sector_single_size() {
        let config = FlashAlgorithm {
            flash_properties: FlashProperties {
                sectors: vec![SectorDescription {
                    size: 0x100,
                    address: 0x0,
                }],
                address_range: 0x1000..0x1000 + 0x1000,
                page_size: 0x10,
                ..Default::default()
            },
            ..Default::default()
        };

        let expected_first = SectorInfo {
            base_address: 0x1000,
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
        let config = FlashAlgorithm {
            flash_properties: FlashProperties {
                sectors: vec![SectorDescription {
                    size: 258,
                    address: 0x0,
                }],
                address_range: 0x800_0000..0x800_0000 + 258 * 10,
                page_size: 0x10,
                ..Default::default()
            },
            ..Default::default()
        };

        let expected_first = SectorInfo {
            base_address: 0x800_0000,
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
        let config = FlashAlgorithm {
            flash_properties: FlashProperties {
                sectors: vec![
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
                ],
                address_range: 0x800_0000..0x800_0000 + 0x10_0000,
                page_size: 0x10,
                ..Default::default()
            },
            ..Default::default()
        };

        let expected_a = SectorInfo {
            base_address: 0x800_4000,
            size: 0x4000,
        };

        let expected_b = SectorInfo {
            base_address: 0x801_0000,
            size: 0x1_0000,
        };

        let expected_c = SectorInfo {
            base_address: 0x80A_0000,
            size: 0x2_0000,
        };

        assert_eq!(Some(expected_a), config.sector_info(0x800_4000));
        assert_eq!(Some(expected_b), config.sector_info(0x801_0000));
        assert_eq!(Some(expected_c), config.sector_info(0x80A_0000));
    }

    #[test]
    fn flash_sector_multiple_sizes_iter() {
        let config = FlashAlgorithm {
            flash_properties: FlashProperties {
                sectors: vec![
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
                ],
                address_range: 0x800_0000..0x800_0000 + 0x8_0000,
                page_size: 0x10,
                ..Default::default()
            },
            ..Default::default()
        };

        let got: Vec<SectorInfo> = config.iter_sectors().collect();

        let expected = &[
            SectorInfo {
                base_address: 0x800_0000,
                size: 0x4000,
            },
            SectorInfo {
                base_address: 0x800_4000,
                size: 0x4000,
            },
            SectorInfo {
                base_address: 0x800_8000,
                size: 0x4000,
            },
            SectorInfo {
                base_address: 0x800_c000,
                size: 0x4000,
            },
            SectorInfo {
                base_address: 0x801_0000,
                size: 0x1_0000,
            },
            SectorInfo {
                base_address: 0x802_0000,
                size: 0x2_0000,
            },
            SectorInfo {
                base_address: 0x804_0000,
                size: 0x2_0000,
            },
            SectorInfo {
                base_address: 0x806_0000,
                size: 0x2_0000,
            },
        ];
        assert_eq!(&got, expected);
    }
}
