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

// Functionality `cfg`d out on platforms with OS/libstd.
#[cfg(target_os = "none")]
mod imp {
    use core::panic::PanicInfo;

    use cortex_m::asm;
    use cortex_m_rt::{exception, ExceptionFrame};

    #[panic_handler]
    fn panic(_: &PanicInfo) -> ! {
        // Trigger a `HardFault`.
        // FIXME: This will actually cause a `UsageFault` if that's enabled!
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
