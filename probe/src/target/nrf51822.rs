use super::*;
use crate::flash::*;

pub fn nRF51822() -> TargetInfo {
    TargetInfo {
        flash_algorithm: FlashAlgorithm {
            load_address: 0x20000000,
            instructions: &[
                0xE00ABE00, 0x062D780D, 0x24084068, 0xD3000040, 0x1E644058, 0x1C49D1FA, 0x2A001E52, 0x4770D1F2,
                0x03004601, 0x28200e00, 0x0940d302, 0xe0051d00, 0xd3022810, 0x1cc00900, 0x0880e000, 0xd50102c9,
                0x43082110, 0x48424770, 0x60414940, 0x60414941, 0x60012100, 0x22f068c1, 0x60c14311, 0x06806940,
                0x483ed406, 0x6001493c, 0x60412106, 0x6081493c, 0x47702000, 0x69014836, 0x43110542, 0x20006101,
                0xb5104770, 0x69014832, 0x43212404, 0x69016101, 0x431103a2, 0x49336101, 0xe0004a30, 0x68c36011,
                0xd4fb03db, 0x43a16901, 0x20006101, 0xb530bd10, 0xffb6f7ff, 0x68ca4926, 0x431a23f0, 0x240260ca,
                0x690a610c, 0x0e0006c0, 0x610a4302, 0x03e26908, 0x61084310, 0x4a214823, 0x6010e000, 0x03ed68cd,
                0x6908d4fb, 0x610843a0, 0x060068c8, 0xd0030f00, 0x431868c8, 0x200160c8, 0xb570bd30, 0x1cc94d14,
                0x68eb0889, 0x26f00089, 0x60eb4333, 0x612b2300, 0xe0174b15, 0x431c692c, 0x6814612c, 0x68ec6004,
                0xd4fc03e4, 0x0864692c, 0x612c0064, 0x062468ec, 0xd0040f24, 0x433068e8, 0x200160e8, 0x1d00bd70,
                0x1f091d12, 0xd1e52900, 0xbd702000, 0x45670123, 0x40023c00, 0xcdef89ab, 0x00005555, 0x40003000,
                0x00000fff, 0x0000aaaa, 0x00000201, 0x00000000
            ],
            pc_init: Some(0x20000047),
            pc_uninit: Some(0x20000075),
            pc_program_page: 0x200000fb,
            pc_erase_sector: 0x200000af,
            pc_erase_all: Some(0x20000083),
            static_base: 0x20000000 + 0x00000020 + 0x0000014c,
            begin_stack: 0x20000000 + 0x00000800,
            begin_data: 0x20002000,
            page_buffers: &[0x20003000, 0x20004000],
            min_program_length: Some(2),
            analyzer_supported: true,
            analyzer_address: 0x20002000,
        },
        basic_register_addresses: BasicRegisterAddresses {
            R0, R1, R2, R3, R9, PC, LR, SP,
        },
        memory_map: vec![
            MemoryRegion::Flash(FlashRegion {
                range: 0..0x40000,
                is_boot_memory: true,
                is_testable: true,
                blocksize: 0x400,
                sector_size: 0x400,
                page_size: 0x400,
                phrase_size: 0x400,
                erase_all_weight: ERASE_ALL_WEIGHT,
                erase_sector_weight: ERASE_SECTOR_WEIGHT,
                program_page_weight: PROGRAM_PAGE_WEIGHT,
                erased_byte_value: 0xFF,
                access: Access::RX,
                are_erased_sectors_readable: true,
            }),
            MemoryRegion::Flash(FlashRegion {
                range: 0x10001000..0x10001000 + 0x100,
                is_boot_memory: false,
                is_testable: false,
                blocksize: 0x100,
                sector_size: 0x100,
                page_size: 0x100,
                phrase_size: 0x100,
                erase_all_weight: ERASE_ALL_WEIGHT,
                erase_sector_weight: ERASE_SECTOR_WEIGHT,
                program_page_weight: PROGRAM_PAGE_WEIGHT,
                erased_byte_value: 0xFF,
                access: Access::RX,
                are_erased_sectors_readable: true,
            }),
            MemoryRegion::Ram(RamRegion {
                range: 0x20000000..0x20000000 + 0x4000,
                is_boot_memory: false,
                is_testable: true,
            }),
        ],
    }
}