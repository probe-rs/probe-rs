use super::{GdbErrorExt, RuntimeTarget};

use gdbstub::{
    arch::Arch,
    target::ext::breakpoints::{
        Breakpoints, HwBreakpoint, HwBreakpointOps, HwWatchpoint, HwWatchpointOps, SwBreakpointOps,
        WatchKind,
    },
};
use probe_rs_rpc::core_ops::WireWatchpointAccess;

impl Breakpoints for RuntimeTarget {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        None
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        Some(self)
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

impl HwWatchpoint for RuntimeTarget {
    /// `Z2` (`watch`), `Z3` (`rwatch`), `Z4` (`awatch`): a data watchpoint on every core. A
    /// core without watchpoints refuses it, and GDB reports that the watchpoint can not be set.
    fn add_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        let access = match kind {
            WatchKind::Write => WireWatchpointAccess::Write,
            WatchKind::Read => WireWatchpointAccess::Read,
            WatchKind::ReadWrite => WireWatchpointAccess::ReadWrite,
        };
        for core_info in &self.cores {
            let core = self.session.core(core_info.index);
            if self
                .block_on(core.set_watchpoint(addr, len, access))
                .is_err()
            {
                return Ok(false);
            }
        }
        self.watchpoints.retain(|&(a, _, _)| a != addr);
        self.watchpoints.push((addr, len, kind));
        Ok(true)
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: u64,
        _len: u64,
        _kind: WatchKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        for core_info in &self.cores {
            let core = self.session.core(core_info.index);
            self.block_on(core.clear_watchpoint(addr))
                .into_target_result()?;
        }
        self.watchpoints.retain(|&(a, _, _)| a != addr);
        Ok(true)
    }
}
