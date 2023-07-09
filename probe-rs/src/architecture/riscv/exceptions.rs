pub(crate) mod riscv32 {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error,
    };

    impl<'probe> ExceptionInterface for crate::architecture::riscv::Riscv32<'probe> {
        fn get_exception_info(
            &mut self,
            _stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            todo!("RISC-V 32-bit exception decoding not implemented")
        }
    }
}
