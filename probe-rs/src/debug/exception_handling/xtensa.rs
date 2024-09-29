use crate::{
    debug::{
        exception_handling::{ExceptionInfo, ExceptionInterface},
        DebugError, DebugInfo, DebugRegisters,
    },
    MemoryInterface,
};

pub struct XtensaExceptionHandler;

impl ExceptionInterface for XtensaExceptionHandler {
    fn exception_details(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        // For architectures where the exception handling has not been implemented in probe-rs,
        // this will result in maintaining the current `unwind` behavior, i.e. unwinding will include up
        // to the first frame that was called from an exception handler.
        Ok(None)
    }

    fn calling_frame_registers(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::debug::DebugRegisters,
        _raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, DebugError> {
        Err(DebugError::NotImplemented("calling frame registers"))
    }

    fn raw_exception(
        &self,
        _stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, DebugError> {
        Err(DebugError::NotImplemented("raw exception"))
    }

    fn exception_description(
        &self,
        _raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        Err(DebugError::NotImplemented("exception description"))
    }
}
