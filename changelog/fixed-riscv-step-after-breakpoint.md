A RISC-V core now remembers that a step moved the program counter past an `ebreak`, so a second step after a software breakpoint or a semihosting call no longer skips an instruction.
