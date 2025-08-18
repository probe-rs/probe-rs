#![no_std]
#![no_main]

use crcxx::crc32::{LookupTable256, Crc};
use probe_rs_crc32_builder::crc_config;

// Force the CRC function to be included and not stripped
#[used]
static CRC_FUNCTION_REF: extern "C" fn(u32, u32, u32) -> u32 = calculate_crc32;

// Create a const CRC32 instance with 256-entry lookup table - algorithm defined in crc_config
const CRC32: Crc<LookupTable256> = Crc::<LookupTable256>::new(&crc_config::CRC_ALGORITHM);

/// CRC32 calculation function - this will be called by probe-rs
/// Flash algorithm calling convention:
/// - R0: start address of data to CRC
/// - R1: length in bytes
/// - R2: initial CRC value (usually 0, but we'll use 0xFFFFFFFF for CRC32_BZIP2)
/// - Return: CRC32 result in R0
#[no_mangle]
#[inline(never)]
pub extern "C" fn calculate_crc32(
    start_addr: u32, 
    length: u32, 
    _initial_crc: u32  // Ignored - CRC_32_BZIP2 always starts with 0xFFFFFFFF
) -> u32 {
    // Validate inputs
    if length == 0 {
        return 0xFFFFFFFF; // Return initial value for empty data
    }
    
    // Limit length to reasonable size to prevent timeout
    //let safe_length = if length > 65536 { 65536 } else { length };
    let safe_length = length;
    
    // Create slice from raw pointer - this is the embedded context pattern  
    // probe-rs ensures this memory is accessible before calling us
    let data = unsafe { 
        core::slice::from_raw_parts(start_addr as *const u8, safe_length as usize) 
    };
    
    // Calculate CRC32_BZIP2 using crcxx
    CRC32.compute(data)
}

// Entry point that calls our CRC function to ensure it's linked
#[no_mangle]
pub extern "C" fn _reset() -> ! {
    // Call our CRC function with dummy data to prevent optimization
    let result = calculate_crc32(0x20000000, 4, 0);
    
    // Use the result in a way that can't be optimized away
    let ptr = result as *mut u32;
    unsafe {
        core::ptr::write_volatile(ptr, result);
    }
    
    loop {}
}

// Alternative entry points 
#[no_mangle]
pub extern "C" fn main() -> ! {
    _reset()
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    _reset()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}