use std::any::Any;

use super::session_data;
use crate::cmd::dap_server::debug_adapter::dap::repl_commands::ReplCommand;
use crate::rpc::{Key, RttClient};
use probe_rs_rpc::rtt_client::ScanRegion as WireScanRegion;

/// `(channel number, channel name)` pairs returned while attaching to RTT.
pub(crate) type ChannelNames = Vec<(u32, String)>;
use crate::cmd::dap_server::server::debug_rtt;
use crate::util::rtt::DefmtState;
use probe_rs::CoreStatus;

/// [CoreData] is used to cache data needed by the debugger, on a per-core basis.
pub struct CoreData {
    pub core_index: usize,
    /// Track the last status observed through the RPC backend.
    ///
    /// Periodic RPC status queries detect asynchronous transitions such as a
    /// breakpoint halt and notify the DAP client. Requests that resume or step
    /// the target update this field as part of their adapter bookkeeping so
    /// the next query can identify the subsequent transition without emitting
    /// a duplicate event for the request's own state change.
    pub last_known_status: CoreStatus,
    pub target_name: String,
    /// Metadata-only display cache of the server-owned stack state.
    ///
    /// This must be invalidated before any operation that can change the
    /// target's registers or execution state, and replaced only after a
    /// complete server unwind succeeds.
    pub stack_frames: Vec<probe_rs_debug::stack_frame::StackFrame>,
    pub breakpoints: Vec<session_data::ActiveBreakpoint>,
    pub rtt_scan_ranges: WireScanRegion,
    pub rtt_connection: Option<debug_rtt::RttConnection>,
    /// defmt data of the program binary, parsed on the first RTT attach.
    ///
    /// The parse reads the whole ELF file, thus each RTT attach after a reset
    /// reuses this. The outer `Option` is `None` until the first parse, the
    /// inner `Option` is `None` for a program without defmt data. A new
    /// program binary clears the cache in
    /// [`super::session_data::SessionData::load_rtt_location`].
    pub defmt_state: Option<Option<DefmtState>>,
    /// Cache of the server-side RTT client handle between attach attempts,
    /// so we only call `create_rtt` once per core (RPC backend).
    pub rtt_remote_handle: Option<Key<RttClient>>,
    pub repl_commands: Vec<ReplCommand>,
    pub test_data: Box<dyn Any>,
}

impl CoreData {
    pub(crate) fn invalidate_stack_frame_cache(&mut self) {
        self.stack_frames.clear();
    }

    pub(crate) fn replace_stack_frame_cache(
        &mut self,
        frames: Vec<probe_rs_debug::stack_frame::StackFrame>,
    ) {
        self.stack_frames = frames;
    }
}

#[test]
fn stack_frame_display_cache_is_invalidated_before_replacement() {
    use probe_rs::RegisterValue;
    use probe_rs_debug::{ObjectRef, registers::DebugRegisters, stack_frame::StackFrame};

    fn frame(id: i64) -> StackFrame {
        StackFrame {
            id: ObjectRef::from(id),
            function_name: format!("frame-{id}"),
            source_location: None,
            registers: DebugRegisters::default(),
            pc: RegisterValue::U32(0),
            frame_base: None,
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: None,
        }
    }

    let mut core_data = CoreData {
        core_index: 0,
        last_known_status: CoreStatus::Unknown,
        target_name: String::new(),
        stack_frames: vec![frame(1)],
        breakpoints: vec![],
        rtt_scan_ranges: WireScanRegion::Ranges(vec![]),
        rtt_connection: None,
        defmt_state: None,
        rtt_remote_handle: None,
        repl_commands: vec![],
        test_data: Box::new(()),
    };

    core_data.invalidate_stack_frame_cache();
    assert!(core_data.stack_frames.is_empty());

    core_data.replace_stack_frame_cache(vec![frame(2)]);
    assert_eq!(core_data.stack_frames.len(), 1);
    assert_eq!(core_data.stack_frames[0].id, ObjectRef::from(2));
}
