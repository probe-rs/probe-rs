use gdbstub::arch::{Arch, RegId, Registers, SingleStepGdbBehavior};

// Placeholder type for runtime architecture trait data
pub(crate) enum RuntimeArch {}

/// Generic implementation for a runtime evaluated architecture
impl Arch for RuntimeArch {
    // We can always handle a 64-bit address space
    type Usize = u64;
    type Registers = RuntimeRegisters;
    type BreakpointKind = usize;
    type RegId = RuntimeRegId;

    fn single_step_gdb_behavior() -> SingleStepGdbBehavior {
        SingleStepGdbBehavior::Required
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct RuntimeRegisters {
    pub pc: u64,
    pub regs: Vec<u8>,
}

impl Registers for RuntimeRegisters {
    type ProgramCounter = u64;

    fn pc(&self) -> Self::ProgramCounter {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for b in &self.regs {
            write_byte(Some(*b))
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        self.regs = bytes.to_vec();

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct RuntimeRegId(u16);

impl RegId for RuntimeRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<std::num::NonZeroUsize>)> {
        id.try_into()
            .map(Some)
            .unwrap_or(None)
            .map(|reg_num| (Self(reg_num), None))
    }
}

impl From<RuntimeRegId> for u32 {
    fn from(r: RuntimeRegId) -> Self {
        r.0.into()
    }
}

impl From<RuntimeRegId> for usize {
    fn from(r: RuntimeRegId) -> Self {
        r.0.into()
    }
}
