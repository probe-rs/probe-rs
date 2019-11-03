use probe_rs::probe::flash::flasher::FlashAlgorithm;
use probe_rs::probe::flash::flasher::AlgorithmParseError;
use probe_rs::probe::flash::memory::{
    MemoryRange,
    RamRegion,
};

use goblin::elf::program_header::*;

const FLASH_BLOB_HEADER_SIZE: u32 = 8;
const FLASH_BLOB_HEADER: [u32; FLASH_BLOB_HEADER_SIZE as usize] = [
    0xE00ABE00, 0x062D780D, 0x24084068, 0xD3000040,
    0x1E644058, 0x1C49D1FA, 0x2A001E52, 0x04770D1F
];
const FLASH_ALGO_STACK_SIZE: u32 = 512;

pub fn extract_flash_algo(buffer: &Vec<u8>, blocksize: u32, ram_region: RamRegion) -> Result<FlashAlgorithm, AlgorithmParseError> {
    let mut blob: Vec<u8> = vec![];

    // let mut algo = FlashAlgorithm {
    //     /// Memory address where the flash algo instructions will be loaded to.
    //     pub load_address: u32,
    //     /// List of 32-bit words containing the position-independant code for the algo.
    //     pub instructions: Vec<u32>,
    //     /// Initial value of the R9 register for calling flash algo entry points, which
    //     /// determines where the position-independant data resides.
    //     pub static_base: u32,
    //     /// Initial value of the stack pointer when calling any flash algo API.
    //     pub begin_stack: u32,
    //     /// Base address of the page buffer. Used if `page_buffers` is not provided.
    //     pub begin_data: u32,
    // };

    let mut algo = FlashAlgorithm::default();

    if let Ok(binary) = goblin::elf::Elf::parse(&buffer.as_slice()) {
        // Extract binary blob.
        for ph in &binary.program_headers {
            if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                println!("Found loadable segment containing:");

                let sector: core::ops::Range<u32> =
                    ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

                for sh in &binary.section_headers {
                    if sector.contains_range(
                        &(sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32),
                    ) {
                        // println!("{:?}", &binary.shdr_strtab[sh.sh_name]);
                        // for line in hexdump::hexdump_iter(
                        //     &buffer[sh.sh_offset as usize..][..sh.sh_size as usize],
                        // ) {
                        //     println!("{}", line);
                        // }

                        if &binary.shdr_strtab[sh.sh_name] == "PrgCode" {
                            println!("Addr: {}", ph.p_paddr as u32);
                            blob.extend(&buffer[ph.p_offset as usize..][..ph.p_filesz as usize]);
                        }
                    }
                }
            }
        }

        use scroll::{Pread};
        let mut blob: Vec<u32> = blob
            .chunks(4)
            .map(|bytes| bytes.pread(0).unwrap())
            .collect();

        blob.extend(FLASH_BLOB_HEADER.iter());

        algo.instructions = blob;

        let mut offset = FLASH_ALGO_STACK_SIZE;

        // Stack address
        let addr_stack = ram_region.range.start + offset;
        // Load address
        let addr_load = addr_stack;
        offset += blob.len() as u32 * 4;

        // Data buffer 1
        let addr_data = ram_region.range.start + offset;
        offset += blocksize;

        assert!(offset > ram_region.range.end - ram_region.range.start, "Not enough space for flash algorithm");

        // Data buffer 2
        let addr_data2 = ram_region.range.start + offset;
        offset += blocksize;

        // Determine whether we can use double buffering or not by the remaining RAM region size.
        let page_buffers = if offset > ram_region.range.end - ram_region.range.start {
            vec![addr_data]
        } else {
            vec![addr_data, addr_data2]
        };

        let code_start = addr_load + FLASH_BLOB_HEADER_SIZE;

        algo.load_address = addr_load;

        // Extract the function pointers.
        for sym in binary.syms.iter() {
            let name = &binary.strtab[sym.st_name];
            println!("{}: 0x{:08x?}", name, sym.st_value);

            match name {
                "Init" => algo.pc_init = Some(sym.st_value as u32),
                "UnInit" => algo.pc_uninit = Some(sym.st_value as u32),
                "EraseChip" => algo.pc_erase_all = Some(sym.st_value as u32),
                "EraseSector" => algo.pc_erase_sector = sym.st_value as u32,
                "ProgramPage" => algo.pc_program_page = sym.st_value as u32,
                _ => {},
            }
        }

        algo.page_buffers = page_buffers;
        algo.begin_data = page_buffers[0];
        algo.begin_stack = addr_stack;
        algo.static_base = code_start + rw_start;
        algo.min_program_length = page_size;
        algo.analyzer_supported = false;
    }

    Ok(algo)
}