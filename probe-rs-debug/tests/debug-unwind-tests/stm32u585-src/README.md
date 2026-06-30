# ARMv8-M (Cortex-M33) exception-unwind test firmware

Source for the `stm32u585_nested_exceptions` and `stm32u585_hardfault_fp` test
fixtures. These exercise stack unwinding *through* exception handlers on
ARMv8-M, which has its own exception-frame layout and EXC_RETURN semantics.

The firmware is a minimal `no_std` crate that depends only on `cortex-m`,
`cortex-m-rt` and `panic-halt` — **no HAL**. The unwind tests only need to run
instructions on the core, so there is no clock/peripheral init; the generic
`cortex-m-rt` vector table is used instead of a device crate. This keeps the
checked-in ELFs small. It was flashed and run on an `iot-stm32u585ai` board.

- `nested_exceptions.rs` — builds a deep call chain that passes through **two
  nested exception handlers on the same MSP stack**
  (`main → level_a → level_b → level_c → SVCall → svc_inner → HardFault →
  hf_inner → loop {}`). Unwinding back to `main`/`Reset` requires advancing the
  stack pointer past each exception frame and reading it from the correct stack.

- `hardfault_fp.rs` — faults into a trivial `HardFault { loop {} }` handler that
  never saves R7 (`main → fp_level_a → fp_level_b → *fault* → HardFault`). The
  frame pointer R7 (callee-saved, not in the hardware exception frame) must be
  preserved across the exception boundary rather than overwritten with the CFA.

## Reproducing the fixtures

1. Build (the `.cargo/config.toml` here selects `thumbv8m.main-none-eabihf`):

   ```
   cargo build --release --bin nested_exceptions --bin hardfault_fp
   ```

2. Flash + run on hardware, let the firmware fault and spin in the (innermost)
   handler, then halt the core and dump the top of the stack plus the System
   Control Space (the ARMv8-M exception code reads SCB fault-status registers
   at `0xE000Exxx`):

   ```
   dump 0x200BE000 0x2000  0xE000E000 0x1000  coredump
   ```

   (`probe-rs debug` REPL `dump` command, or `CoreDump::dump_core`.)

The resulting `*.elf` and `*.coredump` are checked in next to this directory and
referenced by the `full_unwind` tests in `probe-rs-debug/src/debug_info.rs`.
