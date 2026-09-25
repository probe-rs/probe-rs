use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use probe_rs_debug::{DebugInfo, StackFrame, VariableCache};

/// Per-session server-owned debug state. The RPC server builds and owns the
/// `VariableCache` trees (locals + statics) and the cached [`DebugInfo`], so
/// that variable expansion and value reads happen next to the target.
#[derive(Default)]
pub struct ServerDebugState {
    /// Cached DWARF for the session, or `None` when the session was attached
    /// without a program binary. Callers that need DWARF must degrade
    /// gracefully; SVD and semihosting state do not depend on it.
    pub debug_info: Option<Arc<DebugInfo>>,
    pub per_core: HashMap<usize, CoreDebugState>,
    /// Per-core semihosting file state, owned server-side so that semihosting
    /// file I/O happens next to the target. Decoupled from [`Self::per_core`]
    /// so it survives stack-frame refreshes on each halt.
    pub semihosting: HashMap<usize, CoreSemihostingState>,
}

#[derive(Default)]
pub struct CoreDebugState {
    pub stack_frames: Vec<StackFrame>,
    pub static_variables: Option<VariableCache>,
    /// Server-side CMSIS-SVD peripheral variable cache for this core.
    /// Populated by the `load_svd` endpoint and queried by the
    /// `scopes`/`variables` endpoints. `None` when no SVD file was
    /// supplied for the core.
    pub svd_variables: Option<crate::rpc::svd::SvdVariableCache>,
}

/// Server-side per-core semihosting state, mirroring the client's
/// `CoreData::semihosting_handles`/`next_semihosting_handle`.
pub struct CoreSemihostingState {
    pub handles: HashMap<u32, SemihostingFile>,
    pub next_handle: u32,
}

/// File descriptor for files opened by the target via semihosting.
pub struct SemihostingFile {
    pub handle: NonZeroU32,
    pub path: String,
    pub mode: &'static str,
}

impl ServerDebugState {
    /// Replace the session's DWARF and invalidate every cache derived from it.
    ///
    /// SVD variables and semihosting handles are independent of the program
    /// binary, so they intentionally survive a debug-info reload.
    pub fn replace_debug_info(&mut self, debug_info: DebugInfo) {
        self.debug_info = Some(Arc::new(debug_info));
        for core_state in self.per_core.values_mut() {
            core_state.clear_dwarf_derived_state();
        }
    }

    pub fn store_core(
        &mut self,
        core_index: usize,
        stack_frames: Vec<StackFrame>,
        static_variables: Option<VariableCache>,
    ) {
        // Preserve the SVD cache across stack-frame refreshes: `store_core`
        // is invoked on every halt, but the SVD cache is built once per
        // session (by `load_svd`) and must survive the refresh.
        let svd_variables = self
            .per_core
            .get(&core_index)
            .and_then(|c| c.svd_variables.clone());
        self.per_core.insert(
            core_index,
            CoreDebugState {
                stack_frames,
                static_variables,
                svd_variables,
            },
        );
    }

    pub fn clear_core(&mut self, core_index: usize) {
        if let Some(core_state) = self.per_core.get_mut(&core_index) {
            core_state.clear_dwarf_derived_state();
        }
    }

    /// Replace or clear a core's SVD-derived peripheral cache.
    pub fn replace_svd(
        &mut self,
        core_index: usize,
        svd_variables: Option<crate::rpc::svd::SvdVariableCache>,
    ) {
        self.per_core.entry(core_index).or_default().svd_variables = svd_variables;
    }

    /// Get the per-core semihosting state, creating it on first access.
    /// Handles start at 1024 to avoid collision with RTT channel numbers.
    pub fn semihosting_state(&mut self, core_index: usize) -> &mut CoreSemihostingState {
        self.semihosting
            .entry(core_index)
            .or_insert_with(|| CoreSemihostingState {
                handles: HashMap::new(),
                next_handle: 1024,
            })
    }
}

impl CoreDebugState {
    fn clear_dwarf_derived_state(&mut self) {
        self.stack_frames.clear();
        self.static_variables = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use probe_rs::RegisterValue;
    use probe_rs_debug::{ObjectRef, registers::DebugRegisters};
    use std::path::PathBuf;

    fn test_debug_info() -> DebugInfo {
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../probe-rs-debug/tests/debug-unwind-tests/nRF52833_xxAA_full_unwind.elf");
        DebugInfo::from_file(binary).unwrap()
    }

    fn state_with_debug_info() -> ServerDebugState {
        let mut state = ServerDebugState::default();
        state.replace_debug_info(test_debug_info());
        state
    }

    fn stack_frame() -> StackFrame {
        StackFrame {
            id: ObjectRef::from(1),
            function_name: "old frame".to_string(),
            source_location: None,
            registers: DebugRegisters::default(),
            pc: RegisterValue::U32(0),
            frame_base: None,
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: None,
        }
    }

    #[test]
    fn replacing_debug_info_invalidates_only_dwarf_derived_state() {
        let mut state = state_with_debug_info();
        let old_debug_info = state.debug_info.clone().unwrap();
        state.per_core.insert(
            0,
            CoreDebugState {
                stack_frames: vec![stack_frame()],
                static_variables: Some(VariableCache::new_static_cache()),
                ..Default::default()
            },
        );
        state.semihosting_state(0);

        state.replace_debug_info(test_debug_info());

        assert!(!Arc::ptr_eq(
            &old_debug_info,
            state.debug_info.as_ref().unwrap()
        ));
        let core_state = state.per_core.get(&0).unwrap();
        assert!(core_state.stack_frames.is_empty());
        assert!(core_state.static_variables.is_none());
        assert!(state.semihosting.contains_key(&0));
    }

    #[test]
    fn clearing_a_core_invalidates_dwarf_derived_state() {
        let mut state = state_with_debug_info();
        state.per_core.insert(
            0,
            CoreDebugState {
                stack_frames: vec![stack_frame()],
                static_variables: Some(VariableCache::new_static_cache()),
                ..Default::default()
            },
        );

        state.clear_core(0);

        let core_state = state.per_core.get(&0).unwrap();
        assert!(core_state.stack_frames.is_empty());
        assert!(core_state.static_variables.is_none());
    }

    #[test]
    fn replacing_or_removing_svd_drops_stale_peripheral_state() {
        let mut state = state_with_debug_info();

        state.replace_svd(0, Some(crate::rpc::svd::SvdVariableCache::new_svd_cache()));
        assert!(state.per_core.get(&0).unwrap().svd_variables.is_some());

        state.replace_svd(0, None);
        assert!(state.per_core.get(&0).unwrap().svd_variables.is_none());
    }
}
