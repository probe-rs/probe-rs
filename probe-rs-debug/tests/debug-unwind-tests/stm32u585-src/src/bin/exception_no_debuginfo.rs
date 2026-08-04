//! Reproduce the "continue unwinding out of an exception handler that has no
//! DWARF unwind info" bug on ARMv8-M (Cortex-M33).
//!
//!     main
//!       -> nodbg_a -> nodbg_b
//!         -> trigger SVC
//!           -> SVCall  (a *naked asm* handler -> no DWARF unwind info)
//!             -> loop {}   (spin so the debugger can halt here)
//!
//! The SVCall handler is written in naked assembly, so the compiler emits no
//! `.debug_frame` CFI for it. When the core is halted inside the handler and we
//! unwind, the *first* frame (the handler itself) has no DWARF unwind info, so
//! the unwinder takes the "no debug info" path.
//!
//! Before the fix, that path never checked whether the current frame was an
//! exception boundary (LR = EXC_RETURN); it computed a bogus PC from the
//! EXC_RETURN value in LR and stopped, so the interrupted call chain
//! (nodbg_b -> nodbg_a -> main) was lost. The fix detects the EXC_RETURN there
//! too and continues unwinding into the interrupted code.
#![no_std]
#![no_main]

use core::arch::naked_asm;
use core::ptr::read_volatile;

use cortex_m_rt::entry;
use panic_halt as _;

#[inline(never)]
#[no_mangle]
fn nodbg_a() {
    nodbg_b();
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[inline(never)]
#[no_mangle]
fn nodbg_b() {
    // Trigger an SVCall exception.
    unsafe { core::arch::asm!("svc 0") };
    unsafe { read_volatile(&0u32 as *const u32) };
}

// Naked SVCall handler: pure assembly, so no DWARF `.debug_frame` is emitted for
// it. It just spins in place (`b .`), keeping LR = EXC_RETURN, so the debugger
// can halt here with the core inside an exception handler that has no unwind
// info. `#[no_mangle]` exports it under the name the cortex-m-rt vector table
// expects for the SVCall exception.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn SVCall() {
    naked_asm!("b .");
}

#[entry]
fn main() -> ! {
    nodbg_a();

    loop {
        cortex_m::asm::nop();
    }
}
