use std::path::Path;

use probe_rs_target::FlashAlgorithmRelocationRange;
use target_gen::parser::extract_flash_algo;

const PIC_ZERO_VMA_FIXTURE: &[u8] = include_bytes!("test_data/pic_zero_vma_flash_algo.elf");
const PIC_NONZERO_VMA_FIXTURE: &[u8] = include_bytes!("test_data/pic_nonzero_vma_flash_algo.elf");

fn relocation_ranges(
    algo: &probe_rs_target::RawFlashAlgorithm,
) -> Vec<FlashAlgorithmRelocationRange> {
    algo.address_relocation_ranges.clone()
}

#[test]
fn extract_flash_algo_pic_zero_vma_fixture() {
    let algo = extract_flash_algo(
        None,
        PIC_ZERO_VMA_FIXTURE,
        Path::new("pic_zero_vma_flash_algo.elf"),
        true,
        false,
    )
    .unwrap();

    assert_eq!(algo.name, "pic_zero_vma_flash_algo");
    assert_eq!(algo.description, "FixtureFlash");
    assert_eq!(algo.load_address, None);
    assert_eq!(algo.pc_init, Some(0x1));
    assert_eq!(algo.pc_uninit, Some(0x5));
    assert_eq!(algo.pc_erase_sector, 0x9);
    assert_eq!(algo.pc_program_page, 0xD);
    assert_eq!(algo.data_section_offset, 0x2000);
    assert_eq!(algo.static_base_offset, Some(0x1000));
    assert_eq!(algo.link_time_base_address, Some(0x0));
    assert_eq!(
        relocation_ranges(&algo),
        vec![FlashAlgorithmRelocationRange {
            offset: 0x1000,
            size: 0x8,
        }]
    );
    assert_eq!(algo.flash_properties.address_range, 0x0800_0000..0x0800_1000);
    assert_eq!(algo.flash_properties.page_size, 0x100);
}

#[test]
fn extract_flash_algo_pic_nonzero_vma_fixture() {
    let algo = extract_flash_algo(
        None,
        PIC_NONZERO_VMA_FIXTURE,
        Path::new("pic_nonzero_vma_flash_algo.elf"),
        true,
        false,
    )
    .unwrap();

    assert_eq!(algo.name, "pic_nonzero_vma_flash_algo");
    assert_eq!(algo.description, "FixtureFlash");
    assert_eq!(algo.load_address, None);
    assert_eq!(algo.pc_init, Some(0x1));
    assert_eq!(algo.pc_uninit, Some(0x5));
    assert_eq!(algo.pc_erase_sector, 0x9);
    assert_eq!(algo.pc_program_page, 0xD);
    assert_eq!(algo.data_section_offset, 0x2000);
    assert_eq!(algo.static_base_offset, Some(0x1000));
    assert_eq!(algo.link_time_base_address, Some(0x4000));
    assert_eq!(
        relocation_ranges(&algo),
        vec![FlashAlgorithmRelocationRange {
            offset: 0x1000,
            size: 0x8,
        }]
    );
    assert_eq!(algo.flash_properties.address_range, 0x0800_0000..0x0800_1000);
    assert_eq!(algo.flash_properties.page_size, 0x100);
}
