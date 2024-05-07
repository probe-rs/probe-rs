#![no_std]
#![no_main]

extern crate panic_halt;

use cortex_m_rt::entry;

#[entry]
unsafe fn entry() -> ! {
    loop {
        continue;
    }
}
