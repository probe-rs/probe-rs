#![no_main]
#![no_std]

use app as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    ack(10, 10);
    app::exit()
}

fn ack(m: u32, n: u32) -> u32 {
    let array = [0u8; 32 * 1024];
    defmt::info!("ack(m={}, n={}, SP={})", m, n, array.as_ptr());
    if m == 0 {
        n + 1
    } else {
        if n == 0 {
            ack(m - 1, 1)
        } else {
            ack(m - 1, ack(m, n - 1))
        }
    }
}
