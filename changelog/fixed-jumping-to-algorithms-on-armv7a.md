Fixed jumping to loaded code on an ARMv7R, where `bx lr` doesn't work when the core is halted but `mov pc, r0` does.
