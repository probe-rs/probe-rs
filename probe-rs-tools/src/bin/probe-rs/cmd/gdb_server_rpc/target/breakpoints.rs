use super::{GdbErrorExt, RuntimeTarget};

use gdbstub::{
    arch::Arch,
    target::ext::breakpoints::{
        Breakpoints, HwBreakpoint, HwBreakpointOps, HwWatchpointOps, SwBreakpointOps,
    },
};

impl Breakpoints for RuntimeTarget {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        None
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        None
    }
}

impl HwBreakpoint for RuntimeTarget {
    fn add_hw_breakpoint(
        &mut self,
        addr: u64,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        for core_info in &self.cores {
            let core = self.session.core(core_info.index);
            self.block_on(core.set_hw_breakpoint(addr))
                .into_target_result()?;
        }

        Ok(true)
    }

    fn remove_hw_breakpoint(
        &mut self,
        addr: u64,
        _kind: <Self::Arch as Arch>::BreakpointKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        for core_info in &self.cores {
            let core = self.session.core(core_info.index);
            self.block_on(core.clear_hw_breakpoints(vec![addr]))
                .into_target_result()?;
        }

        Ok(true)
    }
}
