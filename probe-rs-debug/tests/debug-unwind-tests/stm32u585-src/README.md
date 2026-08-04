# ARMv8-M (Cortex-M33) exception-unwind test firmware

Source for the `stm32u585_nested_exceptions`, `stm32u585_hardfault_fp` and
`stm32u585_exception_no_debuginfo` test fixtures. These exercise stack unwinding
*through* exception handlers on ARMv8-M, which has its own exception-frame layout
and EXC_RETURN semantics.

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

- `exception_no_debuginfo.rs` — triggers SVCall into a **naked-assembly** handler
  that spins (`main → nodbg_a → nodbg_b → SVCall → loop`). When unwinding hits a
  frame with no DWARF unwind info, the unwinder must still recognise an exception
  boundary (LR = EXC_RETURN) and continue into the interrupted code, rather than
  computing a bogus PC from the EXC_RETURN value. To force the "no unwind info"
  path deterministically, the checked-in `.elf` for this fixture has its
  `.debug_frame` section stripped (see below).

- `psp_exception.rs` — switches thread mode to the **process stack (PSP)**, runs a
  call chain there, then takes an SVCall (`main → switch to PSP → psp_a → psp_b →
  SVCall → loop`). The handler runs on MSP while the exception frame is stacked on
  PSP (EXC_RETURN has SPSEL=1), so the unwinder must read the frame from the
  hardware PSP register, not the handler's MSP, to recover the `psp_b → psp_a`
  chain. The process stack lives at the bottom of RAM, so its dump range differs
  from the others (see below).

## Reproducing the fixtures

1. Build (the `.cargo/config.toml` here selects `thumbv8m.main-none-eabihf`):

   ```
   cargo build --release \
       --bin nested_exceptions --bin hardfault_fp \
       --bin exception_no_debuginfo --bin psp_exception
   ```

2. Flash + run on hardware, let the firmware fault and spin in the (innermost)
   handler, then halt the core and dump the top of the stack plus the System
   Control Space (the ARMv8-M exception code reads SCB fault-status registers
   at `0xE000Exxx`):

   ```
   dump 0x200BE000 0x2000  0xE000E000 0x1000  coredump
   ```

   For `psp_exception`, also dump the process stack at the bottom of RAM:

   ```
   dump 0x20000000 0x1000  0x200BE000 0x2000  0xE000E000 0x1000  coredump
   ```

   (`probe-rs debug` REPL `dump` command, or `CoreDump::dump_core`.)

3. For `exception_no_debuginfo`, strip the DWARF call-frame info from the ELF so
   the unwinder is forced down the "no unwind info" path for every frame (the
   function-name info in `.debug_info` / `.debug_line` is kept):

   ```
   arm-none-eabi-objcopy --remove-section .debug_frame \
       exception_no_debuginfo exception_no_debuginfo
   ```

The resulting `*.elf` and `*.coredump` are checked in next to this directory and
referenced by the `full_unwind` tests in `probe-rs-debug/src/debug_info.rs`.
