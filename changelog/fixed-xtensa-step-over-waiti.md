Xtensa: single stepping a `waiti` instruction no longer waits for the step to time out, which could stop the core in an interrupt handler. The debugger emulates the instruction instead.
