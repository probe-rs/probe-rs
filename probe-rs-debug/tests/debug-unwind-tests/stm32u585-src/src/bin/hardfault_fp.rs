//! Reproduce the frame-pointer (R7) corruption bug when unwinding through a
//! simple exception handler on ARMv8-M (Cortex-M33).
//!
//!     main
//!       -> fp_level_a -> fp_level_b
//!         -> *bad write*           (HardFault exception)
//!           -> HardFault { loop {} }  (spin so the debugger can halt here)
//!
//! The HardFault handler is a trivial spin loop that never saves/restores R7
//! (the ARM frame pointer, a callee-saved register), so at the halt point DWARF
//! has no unwind rule for R7.
//!
//! When the unwinder crosses the exception boundary from the handler back to
//! the faulting frame, R7 must be *preserved* (it is callee-saved and is not in
//! the hardware exception frame, which only stacks R0-R3, R12, LR, PC, xPSR).
//! The buggy fallback instead sets R7 = CFA, corrupting it.
//!
//! No HAL: the test only runs instructions on the core, so it uses the generic
//! cortex-m-rt vector table instead of a device crate.
#![no_std]
#![no_main]

use core::ptr::write_volatile;

use cortex_m_rt::{entry, exception};
use panic_halt as _;

#[inline(never)]
#[no_mangle]
fn fp_level_a() {
    fp_level_b();
    // Prevent tail-call so this stays a distinct frame.
    unsafe { core::arch::asm!("nop") };
}

#[inline(never)]
#[no_mangle]
fn fp_level_b() {
    // Faulting store -> HardFault exception.
    unsafe { write_volatile(0xFFFF_FFF0 as *mut u32, 0xDEAD_BEEF) };
    unsafe { core::arch::asm!("nop") };
}

#[exception]
unsafe fn HardFault(_ef: &cortex_m_rt::ExceptionFrame) -> ! {
    // Trivial handler: spin so we can halt here. This does NOT save R7, so at
    // the halt point DWARF has no unwind rule for R7. A correct unwind must
    // preserve R7 across the exception boundary instead of setting it to CFA.
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    fp_level_a();

    loop {
        cortex_m::asm::nop();
    }
}
