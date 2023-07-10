pub(crate) mod riscv32 {
    use crate::core::ExceptionInterface;

    impl<'probe> ExceptionInterface for crate::architecture::riscv::Riscv32<'probe> {}
}
