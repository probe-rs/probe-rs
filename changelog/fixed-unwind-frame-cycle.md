Stack unwinding stops when it reaches a frame it already unwound, so inconsistent unwind information no longer produces a backtrace that repeats until the frame limit.
