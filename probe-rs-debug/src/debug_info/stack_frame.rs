use super::DebugInfo;
use crate::{
    DebugError, DebugRegisters, SourceLocation, StackFrame, VariableCache, VariableName,
    function_die::FunctionDie, get_object_reference, stack_frame::StackFrameInfo,
    unit_info::UnitInfo,
};
use probe_rs::MemoryInterface;

impl DebugInfo {
    /// Creates the unpopulated cache for `function` variables
    pub(crate) fn create_function_scope_cache(
        &self,
        die_cursor_state: &FunctionDie,
        unit_info: &UnitInfo,
    ) -> Result<VariableCache, DebugError> {
        let function_variable_cache = VariableCache::new_dwarf_cache(
            die_cursor_state.function_die.offset(),
            VariableName::LocalScopeRoot,
            unit_info,
        )?;

        Ok(function_variable_cache)
    }

    /// Best-effort way to look up a function name without debuginfo.
    fn get_stackframe_from_symbols(
        &self,
        address: u64,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let Some(mut fn_name) = self.find_symbol(address) else {
            return Ok(vec![]);
        };

        for lang in [
            gimli::DW_LANG_Rust,
            gimli::DW_LANG_C_plus_plus,
            gimli::DW_LANG_C_plus_plus_03,
            gimli::DW_LANG_C_plus_plus_11,
            gimli::DW_LANG_C_plus_plus_14,
        ] {
            if let Some(demangle) = addr2line::demangle(&fn_name, lang) {
                fn_name = crate::language::from_dwarf(lang).compact_debug_name(&demangle);
                break;
            }
        }

        Ok(vec![StackFrame {
            id: get_object_reference(),
            function_name: fn_name,
            source_location: None,
            registers: unwind_registers.clone(),
            pc: unwind_registers.address_to_register_value(address),
            frame_base: None,
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: None,
        }])
    }

    /// Returns a populated (resolved) [`StackFrame`] struct.
    /// This function will also populate the `DebugInfo::VariableCache` with in scope `Variable`s for each `StackFrame`,
    /// while taking into account the appropriate strategy for lazy-loading of variables.
    pub fn get_stackframe_info(
        &self,
        memory: &mut impl MemoryInterface,
        address: u64,
        cfa: Option<u64>,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let Ok((unit_info, functions)) = self.get_function_dies(address) else {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        };
        let Some(first_function) = functions.first() else {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        };

        // The first function is the non-inlined function, and the rest are inlined functions.
        // The frame base only exists for the non-inlined function, so we can reuse it for all the inlined functions.
        let frame_base = first_function.frame_base(
            self,
            memory,
            &StackFrameInfo {
                registers: unwind_registers,
                frame_base: None,
                canonical_frame_address: cfa,
                caller: None,
            },
        )?;

        let frames = self
            .frame_sites(unit_info, &functions, address)
            .into_iter()
            .map(|site| {
                // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
                // Resolve and cache the function variables.
                let local_variables = self
                    .create_function_scope_cache(site.function, unit_info)
                    .inspect_err(|error| {
                        tracing::error!(
                            "Could not resolve function variables. {error}. Continuing..."
                        );
                    })
                    .ok();

                StackFrame {
                    id: get_object_reference(),
                    function_name: site
                        .function_name
                        .unwrap_or_else(|| unknown_function_name(address, unwind_registers)),
                    source_location: site.source_location,
                    registers: unwind_registers.clone(),
                    pc: unwind_registers.address_to_register_value(site.pc),
                    frame_base,
                    is_inlined: site.function.is_inline(),
                    local_variables,
                    canonical_frame_address: cfa,
                }
            })
            .collect();

        Ok(frames)
    }

    /// The position in each of `functions` at `address`, outermost first.
    ///
    /// `functions` is the chain that [`Self::get_function_dies`] returns. A function that has
    /// further inlined functions is at the call site of the next one. The last function is at
    /// `address`.
    fn frame_sites<'f>(
        &self,
        unit_info: &UnitInfo,
        functions: &'f [FunctionDie<'f>],
        address: u64,
    ) -> Vec<FrameSite<'f>> {
        let mut sites = Vec::new();

        for (function_die, next_function) in functions.iter().zip(functions.iter().skip(1)) {
            let function_name = function_die.function_name(self);

            tracing::debug!("UNWIND: Function name: {:?}", function_name);

            assert!(next_function.is_inline());

            let Some(call_site) =
                inlined_call_site(next_function, unit_info, function_name.as_deref())
            else {
                continue;
            };

            tracing::debug!(
                "UNWIND: Callsite for inlined function {:?}",
                next_function.function_name(self)
            );

            let inlined_caller_source_location = next_function.inline_call_location(self);

            tracing::debug!("UNWIND: Call site: {inlined_caller_source_location:?}");

            sites.push(FrameSite {
                function: function_die,
                function_name,
                source_location: inlined_caller_source_location,
                pc: call_site,
            });
        }

        // Handle last function, which contains no further inlined functions
        if let Some(last_function) = functions.last() {
            sites.push(FrameSite {
                function: last_function,
                function_name: last_function.function_name(self),
                source_location: self.get_source_location(address),
                pc: address,
            });
        }

        sites
    }
}

/// The name of a frame at `address` whose function is unknown.
///
/// The address is a hex string, with the width matching the configured size of the datatype
/// used in the `RegisterValue` address.
pub(super) fn unknown_function_name(address: u64, registers: &DebugRegisters) -> String {
    format!(
        "<unknown function @ {address:#0width$x}>",
        width = (registers.get_address_size_bytes() * 2 + 2)
    )
}

/// The position of one frame in a chain of inlined functions.
struct FrameSite<'f> {
    function: &'f FunctionDie<'f>,
    function_name: Option<String>,
    source_location: Option<SourceLocation>,
    pc: u64,
}

/// The call site of the inlined `function`, which is its first instruction. `caller_name` names
/// the function that `function` is inlined into.
fn inlined_call_site(
    function: &FunctionDie,
    unit_info: &UnitInfo,
    caller_name: Option<&str>,
) -> Option<u64> {
    let address_size = unit_info.unit.header.address_size() as u64;

    let Some(low_pc) = function.low_pc() else {
        tracing::warn!("UNWIND: Unknown starting address for inlined function {caller_name:?}.");
        return None;
    };

    if !(low_pc > address_size && low_pc < u32::MAX as u64) {
        tracing::warn!("UNWIND: Unknown call site for inlined function {caller_name:?}.");
        return None;
    }

    Some(low_pc)
}
