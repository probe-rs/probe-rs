Fixed RISC-V's `read_idcode()` reporting a bogus chip identity for any non-RISC-V JTAG target, by validating the mandatory IEEE 1149.1 IDCODE LSB rule instead of accepting any captured value.
