//! Reproduce stack-unwind-through-exception bugs on ARMv8-M (Cortex-M33).
//!
//! This builds a deterministic, deep call chain that passes through TWO nested
//! exception handlers, both running on the same MSP stack:
//!
//!     main
//!       -> level_a -> level_b -> level_c
//!         -> trigger SVC          (SVCall exception, frame #1 on MSP)
//!           -> svc_inner
//!             -> *bad write*       (HardFault exception, frame #2 on MSP)
//!               -> hf_inner
//!                 -> loop {}        (spin so the debugger can halt here)
//!
//! The core spins in the innermost HardFault handler. A debugger halts it and
//! dumps the core. A correct unwinder must walk back up through BOTH exception
//! frames and recover the full chain down to `main` / Reset.
//!
//! Unwinding through 2 nested exceptions on the same stack requires the SP to
//! be advanced past each exception frame after unstacking and the correct SP
//! source to be used for the same-MSP-stack case.
//!
//! No HAL: the test only runs instructions on the core, so it uses the generic
//! cortex-m-rt vector table instead of a device crate.
#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::{entry, exception};
use panic_halt as _;

#[inline(never)]
#[no_mangle]
fn level_a() {
    level_b();
    // Keep the call from being a tail call.
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[inline(never)]
#[no_mangle]
fn level_b() {
    level_c();
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[inline(never)]
#[no_mangle]
fn level_c() {
    // Trigger an SVCall exception (synchronous, takes us into the SVCall handler).
    unsafe { core::arch::asm!("svc 0") };
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[exception]
fn SVCall() {
    svc_inner();
}

#[inline(never)]
#[no_mangle]
fn svc_inner() {
    // While still inside the SVCall handler (on MSP), trigger a *second*
    // exception (HardFault) by writing to an always-faulting address. This
    // produces a second exception frame stacked on the same MSP stack.
    unsafe { write_volatile(0xFFFF_FFF0 as *mut u32, 0xDEAD_BEEF) };
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[exception]
unsafe fn HardFault(_ef: &cortex_m_rt::ExceptionFrame) -> ! {
    hf_inner()
}

#[inline(never)]
#[no_mangle]
fn hf_inner() -> ! {
    // Spin forever so the debugger can halt the core in this exact state, deep
    // inside two nested exception handlers.
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    level_a();

    // Should never get here.
    loop {
        cortex_m::asm::nop();
    }
}
