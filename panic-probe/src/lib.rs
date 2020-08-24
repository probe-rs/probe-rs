//! Panic handler for `probe-run`.
//!
//! When this panic handler is used, panics will make `probe-run` print a backtrace and exit with a
//! non-zero status code, indicating failure. This building block can be used to run on-device
//! tests.
//!
//! This crate also overrides the Cortex-M HardFault handler: Any HardFault will also cause
//! `probe-run` to exit with an error code.
//!
//! **Note**: The panic message (if any) is currently ignored.

#![no_std]

#[cfg(not(cortex_m))]
compile_error!("`panic-probe` only supports Cortex-M targets (thumbvN-none-eabi[hf])");

// Functionality `cfg`d out on platforms with OS/libstd.
#[cfg(target_os = "none")]
mod imp {
    use core::panic::PanicInfo;

    use cortex_m::asm;
    use cortex_m_rt::{exception, ExceptionFrame};

    #[panic_handler]
    fn panic(_: &PanicInfo) -> ! {
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

    #[exception]
    fn HardFault(_: &ExceptionFrame) -> ! {
        loop {
            // Make `probe-run` print the backtrace and exit.
            asm::bkpt();
        }
    }
}
