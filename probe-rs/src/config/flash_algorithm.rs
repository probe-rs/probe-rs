use super::memory::{FlashRegion, RamRegion, SectorDescription, SectorInfo};

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

    pub sectors: Vec<SectorDescription>,
}

impl FlashAlgorithm {
    pub fn sector_info(&self, region: &FlashRegion, address: u32) -> Option<SectorInfo> {
        if !region.range.contains(&address) {
            return None;
        }

        let mut current_address = region.range.start;

        for sd in &self.sectors {
            if address < current_address + sd.total_size() {
                // We found the correct group of sectors
                let sector_index = (address - current_address) / sd.size;

                return Some(SectorInfo {
                    base_address: current_address + sector_index * sd.size,
                    page_size: region.page_size,
                    size: sd.size,
                });
            }

            current_address += sd.total_size()
        }

        None
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RawFlashAlgorithm {
    /// The name of the flash algorithm.
    pub name: String,
    /// The description of the algorithm.
    pub description: String,
    /// Whether this flash algorithm is the default one or not.
    pub default: bool,
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
    /// The offset from the start of RAM to the data section.
    pub data_section_offset: u32,

    pub sectors: Vec<SectorDescription>,
}

impl RawFlashAlgorithm {
    const FLASH_BLOB_HEADER_SIZE: u32 = 8 * 4;
    const FLASH_ALGO_STACK_SIZE: u32 = 512;
    const FLASH_ALGO_STACK_DECREMENT: u32 = 64;
    const FLASH_BLOB_HEADER: [u32; Self::FLASH_BLOB_HEADER_SIZE as usize / 4] = [
        0xE00A_BE00,
        0x062D_780D,
        0x2408_4068,
        0xD300_0040,
        0x1E64_4058,
        0x1C49_D1FA,
        0x2A00_1E52,
        0x0477_0D1F,
    ];

    /// Constructs a complete flash algorithm, tailored to the flash and RAM sizes given.
    pub fn assemble(&self, ram_region: &RamRegion, flash_region: &FlashRegion) -> FlashAlgorithm {
        let mut instructions = Self::FLASH_BLOB_HEADER.to_vec();

        instructions.extend(&self.instructions);

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
            offset += instructions.len() as u32 * 4;

            // Data buffer 1
            addr_data = ram_region.range.start + offset;
            offset += flash_region.page_size;

            if offset <= ram_region.range.end - ram_region.range.start {
                break;
            }
        }

        // Data buffer 2
        let addr_data2 = ram_region.range.start + offset;
        offset += flash_region.page_size;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if offset <= ram_region.range.end - ram_region.range.start {
            vec![addr_data, addr_data2]
        } else {
            vec![addr_data]
        };

        let code_start = addr_load + Self::FLASH_BLOB_HEADER_SIZE;

        FlashAlgorithm {
            name: self.name.clone(),
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
            sectors: self.sectors.clone(),
        }
    }
}

#[test]
fn flash_sector_single_size() {
    let config = FlashAlgorithm {
        sectors: vec![SectorDescription {
            size: 0x100,
            count: 0x10,
        }],
        ..Default::default()
    };

    let region = FlashRegion {
        range: 0x1000..0x2000,
        is_boot_memory: true,
        erased_byte_value: 0xff,
        page_size: 0x10,
        sector_size: 0,
    };

    let expected_first = SectorInfo {
        base_address: 0x1000,
        page_size: 0x10,
        size: 0x100,
    };

    assert!(config.sector_info(&region, 0x1000 - 1).is_none());

    assert_eq!(expected_first, config.sector_info(&region, 0x1000).unwrap());
    assert_eq!(expected_first, config.sector_info(&region, 0x10ff).unwrap());

    assert_eq!(expected_first, config.sector_info(&region, 0x100b).unwrap());
    assert_eq!(expected_first, config.sector_info(&region, 0x10ea).unwrap());
}

#[test]
fn flash_sector_single_size_weird_sector_size() {
    let config = FlashAlgorithm {
        sectors: vec![SectorDescription {
            size: 258,
            count: 4064,
        }],
        ..Default::default()
    };

    let region = FlashRegion {
        range: 0x800_0000..0x810_0000,
        is_boot_memory: true,
        erased_byte_value: 0xff,
        page_size: 0x10,
        sector_size: 0,
    };

    let expected_first = SectorInfo {
        base_address: 0x800_0000,
        page_size: 0x10,
        size: 258,
    };

    assert!(config.sector_info(&region, 0x800_0000 - 1).is_none());

    assert_eq!(
        expected_first,
        config.sector_info(&region, 0x800_0000).unwrap()
    );
    assert_eq!(
        expected_first,
        config.sector_info(&region, 0x800_0000 + 257).unwrap()
    );

    assert_eq!(
        expected_first,
        config.sector_info(&region, 0x800_000b).unwrap()
    );
    assert_eq!(
        expected_first,
        config.sector_info(&region, 0x800_00e0).unwrap()
    );
}

#[test]
fn flash_sector_multiple_sizes() {
    let config = FlashAlgorithm {
        sectors: vec![
            SectorDescription {
                size: 0x4000,
                count: 4,
            },
            SectorDescription {
                size: 0x1_0000,
                count: 1,
            },
            SectorDescription {
                size: 0x2_0000,
                count: 6,
            },
        ],
        ..Default::default()
    };

    let region = FlashRegion {
        range: 0x800_0000..0x810_0000,
        is_boot_memory: true,
        erased_byte_value: 0xff,
        page_size: 0x10,
        sector_size: 0,
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

    assert_eq!(expected_a, config.sector_info(&region, 0x800_4000).unwrap());
    assert_eq!(expected_b, config.sector_info(&region, 0x801_0000).unwrap());
    assert_eq!(expected_c, config.sector_info(&region, 0x80A_0000).unwrap());
}
