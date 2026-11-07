`InstructionSet` has one `Riscv { xlen, c, f }` variant in place of `RV32`, `RV32C`, `RV64` and `RV64C`. probe-rs rejects images built for a RISC-V target with F on cores without F.
