TODO: Temporarily storing this here, but will move it to the docs before finalizing PR.

# What to expect when setting breakpoints, or stepping through code, during a `probe-rs` debug session

## Breakpoints

When requesting a breakpoint in the source code, `probe-rs` will identify the closest available haltpoint where the target is expected to respect a halt request. This is typically limited by memory locations that contain specific machine code instructions, and falls between the prologue or the epilogue of a function.

In these cases `probe-rs` will set the breakpoint on the closest available haltpoint, and respond with the instruction address, the source file, line, and column, where the breakpoint is set. The user can then choose to accept the proposed location, or unset it and choose a more suitable location.

Some examples where the breakpoint will not be set exactly where the user expects:

- [x] Requesting a breakpoint at the first column of a source line will almost certainly result in the breakpoint being set at a different (the actual) column where the instruction starts. e.g. `let pac = Peripherals::take().unwrap();` will set the first breakpoint at the column that starts the instruction `Peripherals`. In other words, variable binding itself will be the second valid haltpoint, while the code that calculates the value of the variable is considered the first haltpoint.
- [ ] Requesting a breakpoint on a line that falls inside a function signature, will set a breakpoint on the first available instruction in the body of the function.
- [ ] Requesting a breakpoint on a line that contains code which is doesn't result in machine code instructions, e.g. `let apb_freq;`, will set the breakpoint on the next available instruction in the function.
- [ ] Requesting a breakpoint on a line that calls a macro, an inlined function, or any other function, will set a breakpoint on the instruction immediately before the called code, to allow the user to step into the called code after a halt.
- [ ] Requesting a breakpoint on a line that contains "non code" (blank line, comments, conditional compile, other config attributes, etc.), will NOT set any breakpoints, and will warn the user to that effect.

## Stepping

The stepping actions are based on available requests and step granularities defined by the [DAP protocol](https://microsoft.github.io/debug-adapter-protocol/specification#Types_SteppingGranularity).

The stepping implementation shares logic with the [breakpoint](#breakpoints) discussion above, and many of the same rules apply with respect to indentifying valid halt points.

In all cases, it is important to remember that stepping is an implied request to 'run' the target to some future haltpoint that is not necessarily the immediate next instruction. The implication of this is that other breakpoints, code branching, exceptions, interrupts, and other pre-emptions, may result in the processor executing code, and possibly halting on an instruction that is not the target of the user's step action. This is consistent with other debuggers like 'gdb' and 'lldb'.

### Stepping at 'instruction' level

The user must be in an interface that supports viewing disassembled code, e.g. VSCode'a 'Disassembly View`. In this view, all stepping instructions are automatically done on a 'instruction-by-instruction' basis.

### Stepping at 'statement' level

When a user requests a stepping action, while inside non-assembly code (e.g. Rust or C), the stepping instructions are automatically done on a 'statement-by-statement' basis. Most modern programming languages allow multiple 'statements' on a single line of source code. e.g. `let result_value = call_a_function(call_another_function_to_get_parameter(another-parameter));`, constitutes a 'statement' for each of the function calls, as well as one for binding the result to the variable.

#### 'step-over' (a.k.a. 'next')

- [ ] Stepping over a statement that calls a non-inlined function, will run until the function returns before halting. If your code is such that the called function runs a long time, the stepping action may appear to not return control to you, but it really is just waiting for the next halt.
- [x] Stepping over a statement, which is followed by a statement that calls an inlined function, the target will step to the first statement in the inlined function, since logically, that is the next statement in the current sequence.

#### 'step-into'

- [ ] Stepping into a statement that does not call function, will simply step over the statement.
- [x] Stepping into a statement that calls a non-inlined function, will halt at the first instruction after the prologue of the function.
- [ ] Stepping into a statement that calls an inlined function will step over that statement, because inlined code would have already been executed at that point.

#### 'step-out'

- [ ] Stepping out of a no-return function, e.g. `fn main() -> !` is not possible, and will display a message to that effect.
- [ ] For other functions, the halt target will be the first statement after the statement that called the function.
- [ ] In some circumstances where the debug info does not have enough information to complete this request in one step, the target will insert an interrim haltpoint at the last valid instruction in the current function.

#### 'run to line'

- [ ] Debugging environments like VSCode will use breakpoints to implement 'run to line' capabilities, and will result in the processor running to the haltpoint closest to the requested line, using the same rules as discussed in the [breakpoint](#breakpoints) section above.
- [ ] If no hardware breakpoints are available to support this request, VSCode will silently ignore the error, and will run to the next user defined haltpoint.
