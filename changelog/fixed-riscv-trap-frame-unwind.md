Stack unwinding recovers the interrupted frame of a RISC-V trap from mepc and the trap frame, so a backtrace through a trap handler no longer skips the faulting function.
