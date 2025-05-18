Fixed jumping to loaded code on an ARMv7R, where `bx lr` doesn't always seem to work but `mov pc, r0` does
