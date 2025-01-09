#![no_main]
#![no_std]

use core::panic::PanicInfo;

use cortex_m_rt::entry;
use embedded_hal::{blocking::delay::DelayMs, digital::v2::OutputPin};
use microbit::{board::Board, hal::timer::Timer};

#[entry]
fn main() -> ! {
    let mut board = Board::take().unwrap();

    let mut timer = Timer::new(board.TIMER0);

    let _ = board.display_pins.col1.set_low();
    let mut row1 = board.display_pins.row1;

    loop {
        let _ = row1.set_low();
        timer.delay_ms(1_000_u16);
        let _ = row1.set_high();
        timer.delay_ms(1_000_u16);
    }
}

#[inline(never)]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        cortex_m::asm::udf();
    }
}
