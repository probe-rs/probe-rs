use crate::probe::flash::flasher::FlashAlgorithm;
use crate::probe::flash::flasher::AlgorithmParseError;
use crate::probe::flash::memory::MemoryRange;

use goblin::elf::program_header::*;

pub fn extract_flash_algo(buffer: &Vec<u8>) -> Result<FlashAlgorithm, AlgorithmParseError> {
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
    }

    use scroll::{ctx, Pread, LE};
    let blob: Vec<u32> = blob
        .chunks(4)
        .map(|bytes| bytes.pread(0).unwrap())
        .collect();

    algo.instructions = blob;

    Ok(algo)
}