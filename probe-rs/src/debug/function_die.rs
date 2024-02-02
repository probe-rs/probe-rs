use crate::{debug::stack_frame::StackFrameInfo, MemoryInterface};

use super::{
    debug_info, extract_file,
    unit_info::{ExpressionResult, UnitInfo},
    ColumnType, DebugError, DebugRegisters, SourceLocation, VariableLocation,
};

pub(crate) type Die<'abbrev, 'unit> =
    gimli::DebuggingInformationEntry<'abbrev, 'unit, debug_info::GimliReader, usize>;

/// Reference to a DIE for a function
#[derive(Clone)]
pub(crate) struct FunctionDie<'abbrev, 'unit, 'unit_info> {
    pub(crate) unit_info: &'unit_info UnitInfo,

    pub(crate) function_die: Die<'abbrev, 'unit>,

    /// Only present for inlined functions, where this is a reference
    /// to the declaration of the function.
    pub(crate) abstract_die: Option<Die<'abbrev, 'unit>>,
    /// The address of the first instruction in this function.
    pub(crate) low_pc: u64,
    /// The address of the first instruction after this function.
    pub(crate) high_pc: u64,
}

impl<'debugunit, 'abbrev, 'unit: 'debugunit, 'unit_info> FunctionDie<'abbrev, 'unit, 'unit_info> {
    /// Create a new function DIE reference.
    pub(crate) fn new(die: Die<'abbrev, 'unit>, unit_info: &'unit_info UnitInfo) -> Option<Self> {
        let tag = die.tag();

        let gimli::DW_TAG_subprogram = tag else {
            // We only need DIEs for functions, so we can ignore all other DIEs.
            return None;
        };
        Some(Self {
            unit_info,
            function_die: die,
            abstract_die: None,
            low_pc: 0,
            high_pc: 0,
        })
    }

    /// Creates a new inlined function DIE reference.
    pub(crate) fn new_inlined(
        concrete_die: Die<'abbrev, 'unit>,
        abstract_die: Die<'abbrev, 'unit>,
        unit_info: &'unit_info UnitInfo,
    ) -> Option<Self> {
        let tag = concrete_die.tag();

        let gimli::DW_TAG_inlined_subroutine = tag else {
            // We only need DIEs for inlined functions, so we can ignore all other DIEs.
            return None;
        };
        Some(Self {
            unit_info,
            function_die: concrete_die,
            abstract_die: Some(abstract_die),
            low_pc: 0,
            high_pc: 0,
        })
    }

    /// Returns whether this is an inlined function DIE reference.
    pub(crate) fn is_inline(&self) -> bool {
        self.abstract_die.is_some()
    }

    /// Returns the function name described by the die.
    pub(crate) fn function_name(&self, debug_info: &super::DebugInfo) -> Option<String> {
        let Some(fn_name_attr) = self.attribute(gimli::DW_AT_name) else {
            tracing::debug!("DW_AT_name attribute not found, unable to retrieve function name");
            return None;
        };
        let value = fn_name_attr.value();
        let gimli::AttributeValue::DebugStrRef(fn_name_ref) = value else {
            tracing::debug!("Unexpected attribute value for DW_AT_name: {:?}", value);
            return None;
        };
        match debug_info.dwarf.string(fn_name_ref) {
            Ok(fn_name_raw) => Some(String::from_utf8_lossy(&fn_name_raw).to_string()),
            Err(error) => {
                tracing::debug!("No value for DW_AT_name: {:?}: error", error);

                None
            }
        }
    }

    /// Get the call site of an inlined function.
    ///
    /// If this function is not inlined (`is_inline()` returns false),
    /// this function returns `None`.
    pub(crate) fn inline_call_location(
        &self,
        debug_info: &super::DebugInfo,
    ) -> Option<SourceLocation> {
        if !self.is_inline() {
            return None;
        }

        let file_name_attr = self.attribute(gimli::DW_AT_call_file)?;

        let (directory, file) =
            extract_file(debug_info, &self.unit_info.unit, file_name_attr.value())?;
        let line = self
            .attribute(gimli::DW_AT_call_line)
            .and_then(|line| line.udata_value());

        let column =
            self.attribute(gimli::DW_AT_call_column)
                .map(|column| match column.udata_value() {
                    None => ColumnType::LeftEdge,
                    Some(c) => ColumnType::Column(c),
                });
        Some(SourceLocation {
            line,
            column,
            file: Some(file),
            directory: Some(directory),
            low_pc: Some(self.low_pc as u32),
            high_pc: Some(self.high_pc as u32),
        })
    }

    /// Resolve an attribute by looking through both the origin or abstract die entries.
    pub(crate) fn attribute(
        &self,
        attribute_name: gimli::DwAt,
    ) -> Option<debug_info::GimliAttribute> {
        let attribute = self
            .function_die
            .attr(attribute_name)
            .map_or(None, |attribute| attribute);

        if attribute.is_some() {
            return attribute;
        }

        // For inlined function, the *abstract instance* has to be checked if we cannot find the
        // attribute on the *concrete instance*.
        if self.is_inline() {
            if let Some(origin) = self.abstract_die.as_ref() {
                // Try to get the attribute directly
                match origin
                    .attr(attribute_name)
                    .map_or(None, |attribute| attribute)
                {
                    Some(attribute) => return Some(attribute),
                    None => {
                        let specification_attr =
                            origin.attr(gimli::DW_AT_specification).ok().flatten()?;

                        match specification_attr.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                if let Ok(specification) = self.unit_info.unit.entry(unit_ref) {
                                    return specification
                                        .attr(attribute_name)
                                        .map_or(None, |attribute| attribute);
                                }
                            }
                            other_value => tracing::warn!(
                                "Unsupported DW_AT_speficiation value: {:?}",
                                other_value
                            ),
                        }
                    }
                }
            }
        }

        None
    }

    /// Try to retrieve the frame base for this function
    pub fn frame_base(
        &self,
        debug_info: &super::DebugInfo,
        memory: &mut impl MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<u64>, DebugError> {
        match self.unit_info.extract_location(
            debug_info,
            &self.function_die,
            &VariableLocation::Unknown,
            memory,
            StackFrameInfo {
                registers: stackframe_registers,
                frame_base: None,
                canonical_frame_address: None,
            },
        )? {
            ExpressionResult::Location(VariableLocation::Address(address)) => Ok(Some(address)),
            _ => Ok(None),
        }
    }
}
