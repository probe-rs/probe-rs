
#![no_main]
#![no_std]

use panic_halt;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    if let Some(p) = microbit::Peripherals::take() {
        p.GPIO.pin_cnf[4].write(|w| w.dir().output());
        p.GPIO.pin_cnf[13].write(|w| w.dir().output());

        p.GPIO.out.write(|w| unsafe { w.bits(1 << 13) });

        let mut count: u8 = 0;
        loop {
            count += 1;

            if count & 1 == 1 {
                p.GPIO.out.write(|w| unsafe { w.bits(1 << 13) });
            } else {
                p.GPIO.out.write(|w| unsafe { w.bits(0) });
            }

            for _ in 0..1_000_000 {
                cortex_m::asm::nop();
            }
        }
    };

    loop {
        continue;
    }
}