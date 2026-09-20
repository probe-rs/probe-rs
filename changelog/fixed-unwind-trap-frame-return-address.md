Stack unwinding uses the CIE return-address column and skips the call adjustment for a CSR-sourced address, so a trap-handler frame points at the interrupted instruction.
