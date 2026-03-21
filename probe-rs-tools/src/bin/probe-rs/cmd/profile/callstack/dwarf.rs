use probe_rs::CoreType;
use probe_rs::InstructionSet;
use probe_rs::MemoryInterface;
use probe_rs_debug::DebugRegisters;

use super::FunctionAddress;

/// Part of dwarf unwind that is generic for memory interface, used for dwarf_unwind
/// implementation and testing with core dumps.
fn dwarf_unwind_memory_interface(
    memory: &mut impl MemoryInterface,
    debug_info: &probe_rs_debug::DebugInfo,
    initial_registers: DebugRegisters,
    core_type: CoreType,
    instruction_set: Option<InstructionSet>,
) -> Vec<FunctionAddress> {
    let exception_handler = probe_rs_debug::exception_handler_for_core(core_type);
    let stack_frames = debug_info
        .unwind(
            memory,
            initial_registers,
            exception_handler.as_ref(),
            instruction_set,
            usize::MAX,
        )
        .unwrap_or_else(|_| {
            tracing::warn!("Unable to unwind, recording empty sample");
            Vec::new()
        });

    // filter out inlined functions since they do not need to be recorded (they can be added at
    // symbolication time)
    // reverse callstack so root node is first
    let function_addresses: Vec<FunctionAddress> = stack_frames
        .into_iter()
        .enumerate()
        .filter(|(_, frame)| !frame.is_inlined)
        .map(|(idx, frame)| {
            let registers = &frame.registers;
            let pc_reg = registers
                .get_program_counter()
                .expect("A register with PC role exists")
                .value
                .unwrap_or(frame.pc);
            let addr: u64 = pc_reg.try_into().expect("Can convert PC reg to u64");

            match idx {
                0 => FunctionAddress::ProgramCounter(addr),
                _ => FunctionAddress::AdjustedReturnAddress(addr),
            }
        })
        .rev()
        .collect();

    function_addresses
}

/// Use DWARF information to determine the current callstack instruction addresses
pub(crate) fn dwarf_unwind<'a>(
    core: &mut probe_rs::Core<'a>,
    debug_info: &probe_rs_debug::DebugInfo,
) -> Vec<FunctionAddress> {
    let debug_registers = DebugRegisters::from_core(core);
    let core_type = core.core_type();
    let instruction_set = core.instruction_set().ok();

    dwarf_unwind_memory_interface(
        core,
        debug_info,
        debug_registers,
        core_type,
        instruction_set,
    )
}

#[cfg(test)]
mod test {
    use probe_rs_debug::{DebugError, DebugInfo, DebugRegisters};

    use probe_rs::CoreDump;

    use super::super::test::{addresses_to_callstack, coredump_path, get_path_for_test_files};
    use super::*;

    /// Load the DebugInfo from the `elf_file` for the test.
    /// `elf_file` should be the name of a file (or relative path) in the `tests` directory.
    fn load_test_elf_as_debug_info(elf_file: &str) -> DebugInfo {
        let path = get_path_for_test_files(elf_file);
        DebugInfo::from_file(&path).unwrap_or_else(|err: DebugError| {
            panic!("Failed to open file {}: {:?}", path.display(), err)
        })
    }

    /// Like `dwarf_unwind` but for CoreDump rather than Core
    fn dwarf_unwind_core_dump(
        core_dump: &mut CoreDump,
        debug_info: &probe_rs_debug::DebugInfo,
    ) -> Vec<FunctionAddress> {
        let initial_registers = DebugRegisters::from_coredump(core_dump);
        let core_type = core_dump.core_type();
        let instruction_set = core_dump.instruction_set();

        dwarf_unwind_memory_interface(
            core_dump,
            debug_info,
            initial_registers,
            core_type,
            Some(instruction_set),
        )
    }

    fn check_dwarf_unwind(test_name: &str, expect: &Vec<FunctionAddress>) {
        let coredump_path = coredump_path(test_name);

        let mut core_dump = CoreDump::load(&coredump_path).unwrap();
        let debug_info = load_test_elf_as_debug_info(&format!("{test_name}.elf"));

        let res = dwarf_unwind_core_dump(&mut core_dump, &debug_info);

        assert_eq!(&res, expect);
    }

    /// dwarf_unwind RISC-V coredump in ELF format from esp32c6
    #[test]
    fn test_dwarf_unwind_riscv32() {
        let test_name = "esp32c6_coredump_elf";
        // I'm not sure what causes the repeated addresses or if they are correct
        // They are not present in gdb's backtrace
        let expect = addresses_to_callstack(&[
            0x4200124e, // rust_begin_unwind
            0x420054f2, // _ZN4core9panicking9panic_fmt17h021b089f2ed24437E
            0x420054f2, // _ZN4core9panicking9panic_fmt17h021b089f2ed24437E
            0x420054f2, // _ZN4core9panicking9panic_fmt17h021b089f2ed24437E
            0x42000202, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17hcf2d0b9f6da05190E
            0x42000202, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17hcf2d0b9f6da05190E
            0x420052ec, // _ZN16embassy_executor3raw8Executor4poll17h95bc77c9558ed726E
            0x42000244, // _ZN15esp_hal_embassy8executor6thread8Executor3run17h70decec90d969805E
            0x42000510, // main
            0x4200438c, // hal_main
            0x42000132, // _start_rust
        ]);
        check_dwarf_unwind(test_name, &expect);
    }

    /// dwarf_unwind Armv7-em coredump from atsamd51p19a
    #[test]
    fn test_dwarf_unwind_armv7em() {
        let test_name = "atsamd51p19a";
        let expect = addresses_to_callstack(&[
            0x1474, // print_const_pointers
            0x14da, // print_pointers
            0x1538, // main
            0x978,  // Reset_Handler
            0x0,
        ]);
        check_dwarf_unwind(test_name, &expect);
    }

    /// dwarf_unwind Xtensa coredump from esp32s3
    #[test]
    fn test_dwarf_unwind_xtensa() {
        let test_name = "esp32s3_coredump_elf";
        let expect = addresses_to_callstack(&[
            0x420045e3, // rust_begin_unwind
            0x4200587a, // _ZN4core9panicking9panic_fmt17ha467770bc7545c4aE
            0x42000f69, // _ZN11coredump_c67do_loop17hf978f6cd1e9a91bbE
            0x42000d10, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17h82f24e86eebf8c70E.llvm.2709420154441022049
            0x42004c4e, // _ZN16embassy_executor3raw8Executor4poll17h6968ad0e84efef64E
            0x42000bb9, // _ZN15esp_hal_embassy8executor6thread8Executor3run17h3be5e460a364c27eE
            0x42000f7f, // main
            0x42004483, // Reset
        ]);
        check_dwarf_unwind(test_name, &expect);
    }
}
