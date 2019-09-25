use probe_rs_debug::debug::DebugInfo;

use std::fs;
use std::path::Path;


fn setup_test_debug_info(debug_info: &Path) -> DebugInfo  {
    let debug_data = fs::File::open(&debug_info).ok() 
                        .and_then(|file| unsafe { memmap::Mmap::map(&file).ok() });

    debug_data.as_ref().map( |mmap| DebugInfo::from_raw(&*mmap)).expect("Failed to load debug information")
}


#[test]
fn find_unit_by_path() {
    let unit_path = Path::new("/home/dominik/Coding/microbit/examples/gpio_hal_blinky.rs");

    let debug_elf = Path::new("./tests/gpio_hal_blinky");
    
    let debug_info    = setup_test_debug_info(debug_elf.into());



    assert_eq!(0x93a, debug_info.get_breakpoint_location(&unit_path, 14).unwrap().unwrap());
}


#[test]
fn find_non_existing_unit_by_path() {
    let unit_path = Path::new("/home/dominik/Coding/microbit/non_existing.rs");

    let debug_elf = Path::new("./tests/gpio_hal_blinky");
    
    let debug_info    = setup_test_debug_info(debug_elf.into());



    assert!(debug_info.get_breakpoint_location(&unit_path, 14).unwrap().is_none());
}