use super::FlashError;
use crate::{architecture::riscv, core::Architecture, Target};
use probe_rs_target::{
    FlashProperties, PageInfo, RamRegion, RawFlashAlgorithm, SectorInfo, TransferEncoding,
};
use std::mem::size_of_val;

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
    pub load_address: u64,
    /// List of 32-bit words containing the position-independent code for the algo.
    pub instructions: Vec<u32>,
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u64>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u64>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u64,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u64,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u64>,
    /// Initial value of the R9 register for calling flash algo entry points, which
    /// determines where the position-independent data resides.
    pub static_base: u64,
    /// Initial value of the stack pointer when calling any flash algo API.
    pub begin_stack: u64,
    /// A list of base addresses for page buffers. The buffers must be at
    /// least as large as the region's `page_size` attribute. If at least 2 buffers are included in
    /// the list, then double buffered programming will be enabled.
    pub page_buffers: Vec<u64>,

    /// Location of optional RTT control block.
    ///
    /// If this is present, the flash algorithm supports debug output over RTT.
    pub rtt_control_block: Option<u64>,

    /// The properties of the flash on the device.
    pub flash_properties: FlashProperties,

    /// The encoding format accepted by the flash algorithm.
    pub transfer_encoding: TransferEncoding,
}

impl FlashAlgorithm {
    /// Try to retrieve the information about the flash sector
    /// which contains `address`.
    ///
    /// If the `address` is not part of the flash, None will
    /// be returned.
    pub fn sector_info(&self, address: u64) -> Option<SectorInfo> {
        if !self.flash_properties.address_range.contains(&address) {
            tracing::trace!("Address {:08x} not contained in this flash device", address);
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
    pub fn page_info(&self, address: u64) -> Option<PageInfo> {
        if !self.flash_properties.address_range.contains(&address) {
            return None;
        }

        Some(PageInfo {
            base_address: address - (address % self.flash_properties.page_size as u64),
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
            addr += props.page_size as u64;

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

    const FLASH_ALGO_MIN_STACK_SIZE: u32 = 512;

    // Header for RISC-V Flash Algorithms
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

    const XTENSA_FLASH_BLOB_HEADER: [u32; 0] = [];

    /// When the target architecture is not known, and we need to allocate space for the header,
    /// this function returns the maximum size of the header of supported architectures.
    pub fn get_max_algorithm_header_size() -> u64 {
        let algos = [
            Self::algorithm_header(Architecture::Arm),
            Self::algorithm_header(Architecture::Riscv),
            Self::algorithm_header(Architecture::Xtensa),
        ];

        algos.iter().copied().map(size_of_val).max().unwrap() as u64
    }

    fn algorithm_header(architecture: Architecture) -> &'static [u32] {
        match architecture {
            Architecture::Arm => &Self::ARM_FLASH_BLOB_HEADER,
            Architecture::Riscv => &Self::RISCV_FLASH_BLOB_HEADER,
            Architecture::Xtensa => &Self::XTENSA_FLASH_BLOB_HEADER,
        }
    }

    /// Constructs a complete flash algorithm, tailored to the flash and RAM sizes given.
    pub fn assemble_from_raw(
        raw: &RawFlashAlgorithm,
        ram_region: &RamRegion,
        target: &Target,
    ) -> Result<Self, FlashError> {
        Self::assemble_from_raw_with_data(raw, ram_region, ram_region, target)
    }

    /// Constructs a complete flash algorithm, tailored to the flash and RAM sizes given.
    pub fn assemble_from_raw_with_data(
        raw: &RawFlashAlgorithm,
        ram_region: &RamRegion,
        data_ram_region: &RamRegion,
        target: &Target,
    ) -> Result<Self, FlashError> {
        use std::mem::size_of;

        if raw.flash_properties.page_size % 4 != 0 {
            // TODO move to yaml validation
            return Err(FlashError::InvalidPageSize {
                size: raw.flash_properties.page_size,
            });
        }

        let assembled_instructions = raw.instructions.chunks_exact(size_of::<u32>());

        let remainder = assembled_instructions.remainder();
        let last_elem = if !remainder.is_empty() {
            let word = u32::from_le_bytes(
                remainder
                    .iter()
                    .cloned()
                    // Pad with up to three bytes
                    .chain([0u8, 0u8, 0u8])
                    .take(4)
                    .collect::<Vec<u8>>()
                    .try_into()
                    .unwrap(),
            );
            Some(word)
        } else {
            None
        };

        let header = Self::algorithm_header(target.architecture());
        let instructions: Vec<u32> = header
            .iter()
            .copied()
            .chain(
                assembled_instructions.map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())),
            )
            .chain(last_elem)
            .collect();

        let header_size = size_of_val(header) as u64;

        // The start address where we try to load the flash algorithm.
        let addr_load = match raw.load_address {
            Some(address) => {
                // adjust the raw load address to account for the algo header
                address
                    .checked_sub(header_size)
                    .ok_or(FlashError::InvalidFlashAlgorithmLoadAddress { address })?
            }

            None => {
                // assume position independent code
                ram_region.range.start
            }
        };

        if addr_load < ram_region.range.start {
            return Err(FlashError::InvalidFlashAlgorithmLoadAddress { address: addr_load });
        }

        // Memory layout:
        // - Header
        // - Code
        // - Data
        // - Stack

        let code_start = addr_load + header_size;
        let code_size_bytes = (instructions.len() * size_of::<u32>()) as u64;
        let code_end = code_start + code_size_bytes;

        let buffer_page_size = raw.flash_properties.page_size as u64;

        let remaining_ram = ram_region.range.end - code_end;

        // How much space does a data buffer take up in the current RAM region?
        // If the data is in a different region, we don't need to account for it for the stack.
        let buffer_page_size_in_instr_region = if ram_region == data_ram_region {
            buffer_page_size
        } else {
            0
        };

        let stack_size = raw.stack_size.unwrap_or(Self::FLASH_ALGO_MIN_STACK_SIZE) as u64;
        // Make sure at least one data page fits into RAM.
        if buffer_page_size_in_instr_region + stack_size > remaining_ram {
            // The configured stack size is too large. Let's not try to be too clever about it.
            return Err(FlashError::InvalidFlashAlgorithmStackSize);
        }
        tracing::info!("The flash algorithm will be configured with {stack_size} bytes of stack");

        // We have the stack size, let's lay out the data block(s)
        // Determine the bounds of the data region.
        let data_load_addr = if let Some(data_load_addr) = raw.data_load_address {
            data_load_addr
        } else if ram_region == data_ram_region {
            let remaining_ram = remaining_ram - stack_size;

            let data_size = if remaining_ram >= 2 * buffer_page_size {
                2 * buffer_page_size
            } else {
                buffer_page_size
            };

            ram_region.range.end - data_size
        } else {
            data_ram_region.range.start
        };
        let data_range = data_load_addr..data_ram_region.range.end;

        // Now we can place the stack.
        let stack_top_addr = if ram_region == data_ram_region {
            // Under the data...
            data_range.start
        } else {
            // ... or the top of the region if data is in a different region.
            ram_region.range.end
        };
        tracing::info!("Stack top: {:#010X}", stack_top_addr);

        // Data buffer 1
        let first_buffer_start = data_range.start;

        // Data buffer 2
        let second_buffer_start = first_buffer_start + buffer_page_size;
        let second_buffer_end = second_buffer_start + buffer_page_size;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if second_buffer_end <= data_range.end {
            vec![first_buffer_start, second_buffer_start]
        } else {
            vec![first_buffer_start]
        };

        tracing::debug!("Page buffers: {:#010X?}", page_buffers);

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
            begin_stack: stack_top_addr,
            page_buffers,
            rtt_control_block: raw.rtt_location,
            flash_properties: raw.flash_properties.clone(),
            transfer_encoding: raw.transfer_encoding.unwrap_or_default(),
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
