// `lib.rs` is used only for documentation purposes
//! Run embedded programs just like native ones
//!
//! `probe-run` is a custom Cargo runner that transparently runs Rust firmware on a remote device.
//!
//! `probe-run` is powered by [`probe-rs`] and thus supports as many devices and probes as
//! `probe-rs` does.
//!
//! [`probe-rs`]: https://probe.rs/
//!
//! `probe-run` does not currently support the `thumbv7em-none-eabihf` target. As a workaround
//! compile your program for to the `thumbv7em-none-eabi` target. For more details see [issue
//! #1](https://github.com/knurling-rs/probe-run/issues/1).
//!
//! # Setup
//!
//! The recommend way to use `probe-run` is to set as the Cargo runner of your application.
//! Add this line to your Cargo configuration (`.cargo/config`) file:
//!
//!
//! ``` toml
//! [target.'cfg(all(target_arch = "arm", target_os = "none"))']
//! runner = "probe-run --chip $CHIP"
//! ```
//!
//! Instead of `$CHIP` you'll need to write the name of your microcontroller.
//! For example, one would use `nRF52840_xxAA` for the nRF52840 microcontroller.
//! To list all supported chips run `probe-run --list-chips`.
//!
//! You are all set.
//! You can now run your firmware using `cargo run`.
//! For example,
//!
//! ``` rust,ignore
//! use cortex_m::asm;
//! use cortex_m_rt::entry;
//! use rtt_target::rprintln;
//!
//! #[entry]
//! fn main() -> ! {
//!     // omitted: rtt initialization
//!     rprintln!("Hello, world!");
//!     loop { asm::bkpt() }
//! }
//! ```
//!
//! ``` console
//! $ cargo run --bin hello
//! Running `probe-run target/thumbv7em-none-eabi/debug/hello`
//! flashing program ..
//! DONE
//! resetting device
//! Hello, world!
//! stack backtrace:
//! 0: 0x0000031e - __bkpt
//! 1: 0x000001d2 - hello::__cortex_m_rt_main
//! 2: 0x00000108 - main
//! 3: 0x000002fa - Reset
//! ```
//!
//! # Stack backtraces
//!
//! When the firmware reaches a BKPT instruction the device halts.
//! The `probe-run` tool treats this halted state as the "end" of the application and exits with exit-code = 0.
//! Before exiting `probe-run` prints the stack backtrace of the halted program.
//! This backtrace follows the format of the `std` backtraces you get from `std::panic!` but includes `<exception entry>` lines to indicate where an exception/interrupt occurred.
//!
//! ``` rust,ignore
//! use cortex_m::asm;
//! use rtt_target::rprintln;
//!
//! #[entry]
//! fn main() -> ! {
//!     // omitted: rtt initialization
//!     rprintln!("main");
//!     SCB::set_pendsv();
//!     rprintln!("after PendSV");
//!
//!     loop { asm::bkpt() }
//! }
//!
//! #[exception]
//! fn PendSV() {
//!     defmt::info!("PendSV");
//!     asm::bkpt()
//! }
//! ```
//!
//! ``` console
//! $ cargo run --bin exception --release
//! main
//! PendSV
//! stack backtrace:
//! 0: 0x00000902 - __bkpt
//! <exception entry>
//! 1: 0x000004de - nrf52::__cortex_m_rt_main
//! 2: 0x00000408 - main
//! 3: 0x000005ee - Reset
//! ```

#![doc(html_root_url = "https://docs.rs/probe-run/0.1.0")]
