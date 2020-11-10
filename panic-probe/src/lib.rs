//! Panic handler for `probe-run`.
//!
//! When this panic handler is used, panics will make `probe-run` print a backtrace and exit with a
//! non-zero status code, indicating failure. This building block can be used to run on-device
//! tests.
//!
//! # Panic Messages
//!
//! By default, `panic-probe` *ignores* the panic message. You can enable one of the following
//! features to print it instead:
//!
//! - `print-rtt`: Prints the panic message over plain RTT (via `rtt-target`). RTT must be
//!   initialized by the app.
//! - `print-defmt`: Prints the panic message via [defmt]'s transport (note that defmt will not be
//!   used to efficiently format the message).
//!
//! [defmt]: https://github.com/knurling-rs/defmt/

#![no_std]
#![cfg(target_os = "none")]

#[cfg(not(cortex_m))]
compile_error!("`panic-probe` only supports Cortex-M targets (thumbvN-none-eabi[hf])");

// Functionality `cfg`d out on platforms with OS/libstd.
#[cfg(target_os = "none")]
mod imp {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use cortex_m::asm;

    #[cfg(feature = "print-rtt")]
    use crate::print_rtt::print;

    #[cfg(feature = "print-defmt")]
    use crate::print_defmt::print;

    #[cfg(not(any(feature = "print-rtt", feature = "print-defmt")))]
    fn print(_: &core::panic::PanicInfo) {}

    #[panic_handler]
    fn panic(info: &PanicInfo) -> ! {
        static PANICKED: AtomicBool = AtomicBool::new(false);

        cortex_m::interrupt::disable();

        // Guard against infinite recursion, just in case.
        if PANICKED.load(Ordering::Relaxed) {
            loop {
                asm::bkpt();
            }
        }

        PANICKED.store(true, Ordering::Relaxed);

        print(info);

        // Trigger a `HardFault` via `udf` instruction.

        // If `UsageFault` is enabled, we disable that first, since otherwise `udf` will cause that
        // exception instead of `HardFault`.
        #[cfg(not(any(armv6m, armv8m_base)))]
        {
            const SHCSR: *mut u32 = 0xE000ED24usize as _;
            const USGFAULTENA: usize = 18;

            unsafe {
                let mut shcsr = core::ptr::read_volatile(SHCSR);
                shcsr &= !(1 << USGFAULTENA);
                core::ptr::write_volatile(SHCSR, shcsr);
            }
        }

        asm::udf();
    }
}

#[cfg(feature = "print-rtt")]
mod print_rtt {
    use core::panic::PanicInfo;
    use rtt_target::rprintln;

    pub fn print(info: &PanicInfo) {
        rprintln!("{}", info);
    }
}

#[cfg(feature = "print-defmt")]
mod print_defmt {
    use core::{
        cmp,
        fmt::{self, Write},
        mem,
        panic::PanicInfo,
        str,
    };

    const DEFMT_BUF_SIZE: usize = 128;
    const OVERFLOW_MARK: &str = "…";

    struct Sink<'a> {
        buf: &'a mut [u8],
        pos: usize,
        overflowed: bool,
    }

    impl<'a> fmt::Write for Sink<'a> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            if self.overflowed {
                return Ok(());
            }

            let buf = mem::replace(&mut self.buf, &mut []);
            let buf_unused = &mut buf[self.pos..];

            if buf_unused.len() < s.len() {
                self.overflowed = true;
            }

            let lim = cmp::min(buf_unused.len(), s.len());
            buf_unused[..lim].copy_from_slice(&s.as_bytes()[..lim]);
            self.buf = buf;
            self.pos += lim;
            Ok(())
        }
    }

    pub fn print(info: &PanicInfo) {
        let mut buf = [0; DEFMT_BUF_SIZE];
        let mut sink = Sink {
            buf: &mut buf,
            pos: 0,
            overflowed: false,
        };
        write!(sink, "{}", info).ok();

        let msg = str::from_utf8(&sink.buf[..sink.pos]).unwrap_or("<utf-8 error>");
        let overflow = if sink.overflowed { OVERFLOW_MARK } else { "" };
        defmt::error!("{:str}{:str}", msg, overflow);
    }
}
