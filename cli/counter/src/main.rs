#![no_main]
#![no_std]

extern crate cortex_m;
extern crate cortex_m_rt;
extern crate panic_halt;

extern crate stm32f429i_disc as board;

use cortex_m_rt::entry;

use board::hal::delay::Delay;
use board::hal::prelude::*;
use board::hal::stm32;

use cortex_m::peripheral::Peripherals;

#[entry]
fn main() -> ! {
    if let (Some(p), Some(cp)) = (stm32::Peripherals::take(), Peripherals::take()) {
        let gpiod = p.GPIOG.split();

        // (Re-)configure PG13 (green LED) as output
        let mut led = gpiod.pg13.into_push_pull_output();

        // Constrain clock registers
        let mut rcc = p.RCC.constrain();

        // Configure clock to 180 MHz (i.e. the maximum) and freeze it
        let clocks = rcc.cfgr.sysclk(180.mhz()).freeze();

        // Get delay provider
        let mut delay = Delay::new(cp.SYST, clocks);

        let mut counter: u32 = 0;

        loop {
            // Turn LED on
            led.set_high();

            // Delay twice for half a second due to limited timer resolution
            delay.delay_ms(500_u16);
            delay.delay_ms(500_u16);

            core::ptr::write_volatile(0x20010000 as *mut u32, counter);
            counter += 1;

            // Turn LED off
            led.set_low();

            // Delay twice for half a second due to limited timer resolution
            delay.delay_ms(500_u16);
            delay.delay_ms(500_u16);
        }
    }

    loop {
        continue;
    }
}