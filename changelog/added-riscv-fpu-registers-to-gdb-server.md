Add RISC-V FPU registers to the GDB server so targets compiled with
floating-point support correctly advertise the FP capability (`flen`). This
prevents errors like: "bfd requires flen 4, but target has flen 0".

