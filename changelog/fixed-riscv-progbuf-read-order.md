A RISC-V block read through the program buffer without `abstractauto` now waits for the command before it reads `data0`, so a slow target no longer returns the value of the previous word.
