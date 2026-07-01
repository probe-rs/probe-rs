//! Reproduce unwinding through a PSP -> MSP transition on ARMv8-M (Cortex-M33).
//!
//!     main (on MSP)
//!       -> switch thread mode to PSP (CONTROL.SPSEL = 1)
//!         -> psp_a -> psp_b          (running on the process stack, PSP)
//!           -> trigger SVC
//!             -> SVCall handler       (runs on MSP; exception frame is on PSP)
//!               -> psp_handler_inner
//!                 -> loop {}           (spin so the debugger can halt here)
//!
//! The exception is taken while thread mode is using PSP, so the hardware
//! stacks the exception frame on the *process* stack (PSP) while the handler
//! itself runs on MSP. EXC_RETURN therefore has SPSEL = 1.
//!
//! To unwind from the handler back into the interrupted `psp_b -> psp_a` chain,
//! the unwinder must read the exception frame from the hardware PSP register
//! (the frame is on a different stack from the handler), not from the
//! DWARF-unwound generic SP (which is the handler's MSP). This exercises the
//! SPSEL = 1 branch of the ARMv8-M exception unwinding.
#![no_std]
#![no_main]

use core::ptr::{addr_of_mut, read_volatile};

use cortex_m_rt::{entry, exception};
use panic_halt as _;

/// Dedicated process stack (PSP). 8-byte aligned; the SP grows down from the top.
#[repr(align(8))]
struct ProcessStack(#[allow(dead_code)] [u8; 2048]);
static mut PROCESS_STACK: ProcessStack = ProcessStack([0; 2048]);

#[inline(never)]
#[no_mangle]
fn psp_a() {
    psp_b();
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[inline(never)]
#[no_mangle]
fn psp_b() {
    // Trigger an SVCall exception while running on PSP.
    unsafe { core::arch::asm!("svc 0") };
    unsafe { read_volatile(&0u32 as *const u32) };
}

#[exception]
fn SVCall() {
    psp_handler_inner();
}

#[inline(never)]
#[no_mangle]
fn psp_handler_inner() -> ! {
    // Spin forever so the debugger can halt the core inside the handler, which
    // runs on MSP while its exception frame sits on PSP.
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    // Point PSP at the top of our process stack and switch thread mode to use
    // PSP (CONTROL.SPSEL = 1). Done in one asm block so the compiler never tries
    // to touch the stack between loading PSP and the SPSEL switch.
    unsafe {
        let psp_top = addr_of_mut!(PROCESS_STACK) as u32 + 2048;
        core::arch::asm!(
            "msr psp, {top}",       // PSP = top of process stack
            "mrs {tmp}, control",
            "orr {tmp}, {tmp}, #2", // CONTROL.SPSEL = 1 (use PSP in thread mode)
            "msr control, {tmp}",
            "isb",
            top = in(reg) psp_top,
            tmp = out(reg) _,
            options(nostack),
        );
    }

    // From here on, thread mode runs on PSP. Run the call chain and take an
    // exception so the frame is stacked on PSP.
    psp_a();

    loop {
        cortex_m::asm::nop();
    }
}
