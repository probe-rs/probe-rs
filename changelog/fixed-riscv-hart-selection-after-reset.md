probe-rs now tracks the hart that a debug module re-initialization selects, so register and memory access after a reset no longer go to hart 0 of a multi-hart RISC-V target.
