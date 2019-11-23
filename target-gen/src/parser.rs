use probe_rs::config::flash_algorithm::FlashAlgorithm;
use probe_rs::config::memory::RamRegion;

use crate::flash_device::FlashDevice;

const FLASH_BLOB_HEADER_SIZE: u32 = 8 * 4;
const FLASH_BLOB_HEADER: [u32; FLASH_BLOB_HEADER_SIZE as usize / 4] = [
    0xE00A_BE00, 0x062D_780D, 0x2408_4068, 0xD300_0040,
    0x1E64_4058, 0x1C49_D1FA, 0x2A00_1E52, 0x0477_0D1F
];
const FLASH_ALGO_STACK_SIZE: u32 = 512;
const FLASH_ALGO_STACK_DECREMENT: u32 = 64;

pub fn read_elf_bin_data<'a>(elf: &'a goblin::elf::Elf<'_>, buffer: &'a [u8], address: u32, size: u32) -> Option<&'a [u8]> {
    for ph in &elf.program_headers {
        let segment_address = ph.p_paddr as u32;
        let segment_size = ph.p_memsz.min(ph.p_filesz) as u32;
        
        if address > segment_address + segment_size {
            continue;
        }

        if address + size <= segment_address {
            continue;
        }

        if address >= segment_address && address + size <= segment_address + segment_size {
            let start = ph.p_offset as u32 + address - segment_address;
            return Some(&buffer[start as usize..][..size as usize]);
        }
    }

    None
}

pub fn extract_flash_algo(
    mut file: impl std::io::Read,
    file_name: &std::path::Path,
    ram_region: RamRegion,
    default: bool,
) -> (FlashAlgorithm, u32, u32, u8) {
    let mut buffer = vec![];
    file.read_to_end(&mut buffer).unwrap();

    let mut algo = FlashAlgorithm::default();

    let mut flash_device = None;
    if let Ok(elf) = goblin::elf::Elf::parse(&buffer.as_slice()) {
        // Extract the flash device info.
        for sym in elf.syms.iter() {
            let name = &elf.strtab[sym.st_name];

            if let "FlashDevice" = name {
                // This struct contains information about the FLM file structure.
                let address = sym.st_value as u32;
                flash_device = Some(FlashDevice::new(&elf, &buffer, address));
            }
        }

        let flash_device = flash_device.clone().unwrap();

        // Extract binary blob.
        let algorithm_binary = crate::algorithm_binary::AlgorithmBinary::new(&elf, &buffer);

        let mut instructions = FLASH_BLOB_HEADER.to_vec();

        use scroll::{Pread};
        let blob: Vec<u32> = algorithm_binary.blob
            .chunks(4)
            .map(|bytes| bytes.pread(0).unwrap())
            .collect();

        instructions.extend(blob.iter());

        algo.instructions = instructions.clone();

        let mut offset = 0;
        let mut addr_stack = 0;
        let mut addr_load = 0;
        let mut addr_data = 0;

        // Try to find a stack size that fits with at least one page of data.
        for i in 0..FLASH_ALGO_STACK_SIZE / FLASH_ALGO_STACK_DECREMENT {
            offset = FLASH_ALGO_STACK_SIZE - FLASH_ALGO_STACK_DECREMENT * i;
            // Stack address
            addr_stack = ram_region.range.start + offset;
            // Load address
            addr_load = addr_stack;
            offset += instructions.len() as u32 * 4;

            // Data buffer 1
            addr_data = ram_region.range.start + offset;
            offset += flash_device.page_size;

            if offset <= ram_region.range.end - ram_region.range.start {
                break;
            }
        }

        // Data buffer 2
        let addr_data2 = ram_region.range.start + offset;
        offset += flash_device.page_size;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if offset <= ram_region.range.end - ram_region.range.start {
            vec![addr_data, addr_data2]
        } else {
            vec![addr_data]
        };

        let code_start = addr_load + FLASH_BLOB_HEADER_SIZE;

        algo.load_address = addr_load;

        // Extract the function pointers.
        for sym in elf.syms.iter() {
            let name = &elf.strtab[sym.st_name];

            match name {
                "Init" => algo.pc_init = Some(code_start + sym.st_value as u32),
                "UnInit" => algo.pc_uninit = Some(code_start + sym.st_value as u32),
                "EraseChip" => algo.pc_erase_all = Some(code_start + sym.st_value as u32),
                "EraseSector" => algo.pc_erase_sector = code_start + sym.st_value as u32,
                "ProgramPage" => algo.pc_program_page = code_start + sym.st_value as u32,
                _ => {},
            }
        }

        algo.name = file_name.file_stem().unwrap().to_str().unwrap().to_owned();
        algo.default = default;
        algo.page_buffers = page_buffers.clone();
        algo.begin_data = page_buffers[0];
        algo.begin_stack = addr_stack;
        algo.static_base = code_start + algorithm_binary.rw.start;
        algo.min_program_length = Some(flash_device.page_size);
        algo.analyzer_supported = false;
    }
    
    let flash_device = flash_device.unwrap();
    (algo, flash_device.page_size, flash_device.sectors[0].size, flash_device.erased_default_value)
}