Add Live Watch variable modification support to DAP server, enabling runtime
variable value changes without pausing the debugger (similar to Keil µVision).

This implementation includes:
- Queue mechanism for pending variable modifications during debug execution
- Batch application of queued modifications when debugger pauses
- Support for all primitive data types (bool, char, String, i8-i128, u8-u128, f32, f64)
- Comprehensive error handling and modification history tracking
- DAP setVariable request handler with intelligent state management

New files:
- `probe-rs-tools/bin/probe-rs/cmd/dap_server/debug_adapter/dap/set_variable.rs`
- `probe-rs-tools/bin/probe-rs/cmd/dap_server/server/variable_modifier.rs`
