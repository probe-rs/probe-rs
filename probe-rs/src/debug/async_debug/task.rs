#[cfg(test)]
mod tests {

    use test_case::test_case;

    use crate::{
        debug::{
            stack_frame::StackFrameInfo,
            test_helpers::{get_path_for_test_files, load_test_elf_as_debug_info},
        },
        exception_handler_for_core, CoreDump,
    };

    #[test_case("timer-embassy"; "timer embassy")]
    fn full_unwind(test_name: &str) {
        // TODO: Add RISC-V tests.
        let debug_info =
            load_test_elf_as_debug_info(format!("async-tests/{test_name}.elf").as_str());
        let mut adapter = CoreDump::load(&get_path_for_test_files(
            format!("async-tests/{test_name}.coredump").as_str(),
        ))
        .unwrap();
        let snapshot_name = test_name.to_string();

        let initial_registers = adapter.debug_registers();
        // let exception_handler = exception_handler_for_core(adapter.core_type());
        // let instruction_set = adapter.instruction_set();

        // let mut stack_frames = debug_info
        //     .unwind(
        //         &mut adapter,
        //         initial_registers.clone(),
        //         exception_handler.as_ref(),
        //         Some(instruction_set),
        //     )
        //     .unwrap();

        // // Expand and validate the static and local variables for each stack frame.
        // for frame in stack_frames.iter_mut() {
        //     let mut variable_caches = Vec::new();
        //     if let Some(local_variables) = &mut frame.local_variables {
        //         variable_caches.push(local_variables);
        //     }
        //     for variable_cache in variable_caches {
        //         // Cache the deferred top level children of the of the cache.
        //         variable_cache.recurse_deferred_variables(
        //             &debug_info,
        //             &mut adapter,
        //             10,
        //             StackFrameInfo {
        //                 registers: &frame.registers,
        //                 frame_base: frame.frame_base,
        //                 canonical_frame_address: frame.canonical_frame_address,
        //             },
        //         );
        //     }
        // }

        let mut static_variables = debug_info.create_static_scope_cache();

        static_variables.recurse_deferred_variables(
            &debug_info,
            &mut adapter,
            2,
            StackFrameInfo {
                registers: &initial_registers,
                frame_base: None,
                canonical_frame_address: None,
            },
        );

        dbg!(static_variables);

        panic!();

        // Using YAML output because it is easier to read than the default snapshot output,
        // and also because they provide better diffs.
        // insta::assert_yaml_snapshot!(snapshot_name, stack_frames);
    }
}
