use gimli::UnitOffset;
use std::ops::Range;

use crate::{MemoryInterface, stack_frame::StackFrameInfo};

use super::{
    ColumnType, DebugError, DebugInfo, SourceLocation, VariableLocation, debug_info, extract_file,
    unit_info::{ExpressionResult, UnitInfo},
};

pub(crate) type Die = gimli::DebuggingInformationEntry<debug_info::GimliReader, usize>;

/// Reference to a DIE for a function
#[derive(Clone)]
pub(crate) struct FunctionDie<'data> {
    /// A reference to the compilation unit this function belongs to.
    pub(crate) unit_info: &'data UnitInfo,
    /// The DIE (Debugging Information Entry) for the function.
    pub(crate) function_die: Die,
    /// The optional specification DIE for the function, if it has one.
    /// - For regular functions, this applies to the `function_die`.
    /// - For inlined functions, this applies to the `abstract_die`.
    ///
    /// The specification DIE will contain separately declared attributes,
    /// e.g. for the function name.
    /// See DWARF spec, 2.13.2.
    pub(crate) specification_die: Option<Die>,
    /// Only present for inlined functions, where this is a reference
    /// to the declaration of the function.
    pub(crate) abstract_die: Option<Die>,
    /// The address ranges for which this function is valid.
    pub(crate) ranges: Vec<Range<u64>>,
}

impl PartialEq for FunctionDie<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.function_die.offset() == other.function_die.offset()
    }
}

impl<'a> FunctionDie<'a> {
    /// Create a new function DIE reference.
    /// We only return DIE's that are functions, with valid address ranges that represent machine code
    /// relevant to the address/program counter specified.
    /// Other DIE's will return None, and should be ignored.
    pub(crate) fn new(
        function_die: Die,
        unit_info: &'a UnitInfo,
        debug_info: &'a DebugInfo,
        address: u64,
    ) -> Result<Option<Self>, DebugError> {
        let is_inlined_function = match function_die.tag() {
            gimli::DW_TAG_subprogram => false,
            gimli::DW_TAG_inlined_subroutine => true,
            _ => {
                // We only need DIEs for functions, so we can ignore all other DIEs.
                return Ok(None);
            }
        };

        //Validate the function DIE ranges, and confirm this DIE applies to the requested address.
        let mut gimli_ranges = debug_info
            .dwarf
            .die_ranges(&unit_info.unit, &function_die)?;
        let mut die_ranges = Vec::new();
        while let Ok(Some(gimli_range)) = gimli_ranges.next() {
            if gimli_range.begin == 0 {
                // TODO: The DW_AT_subprograms with low_pc == 0 cause overlapping ranges with other 'valid' function dies, and obscures the correct function die.
                // We need to understand what those mean, and how to handle them correctly.
                return Ok(None);
            }
            die_ranges.push(gimli_range.begin..gimli_range.end);
        }
        if !die_ranges.iter().any(|range| range.contains(&address)) {
            return Ok(None);
        }

        let specification_die;

        // For inlined functions, we also need to find the abstract origin.
        let abstract_die = if is_inlined_function {
            let Some(abstract_die) = debug_info.resolve_die_reference(
                gimli::DW_AT_abstract_origin,
                &function_die,
                unit_info,
            ) else {
                tracing::debug!("No abstract origin found for inlined function");
                return Ok(None);
            };
            specification_die = debug_info.resolve_die_reference(
                gimli::DW_AT_specification,
                &abstract_die,
                unit_info,
            );
            Some(abstract_die)
        } else {
            specification_die = debug_info.resolve_die_reference(
                gimli::DW_AT_specification,
                &function_die,
                unit_info,
            );
            None
        };

        Ok(Some(Self {
            unit_info,
            function_die,
            specification_die,
            abstract_die,
            ranges: die_ranges,
        }))
    }

    /// Returns the lowest valid address for which this function DIE is valid.
    /// Please use `range_contains()` to check whether an address is contained in the range.
    pub(crate) fn low_pc(&self) -> Option<u64> {
        self.ranges.first().map(|range| range.start)
    }

    /// Returns the highest valid address for which this function DIE is valid.
    /// Please use `range_contains()` to check whether an address is contained in the range.
    pub(crate) fn high_pc(&self) -> Option<u64> {
        self.ranges.last().map(|range| range.end)
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
            Ok(fn_name_raw) => {
                let function_name = String::from_utf8_lossy(&fn_name_raw);

                let language = crate::language::from_dwarf(self.unit_info.get_language());
                Some(language.format_function_name(function_name.as_ref(), self, debug_info))
            }
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

        let path = extract_file(debug_info, &self.unit_info.unit, file_name_attr.value())?;
        let line = self
            .attribute(gimli::DW_AT_call_line)
            .and_then(|line| line.udata_value());

        let column =
            self.attribute(gimli::DW_AT_call_column)
                .map(|column| match column.udata_value() {
                    None => ColumnType::LeftEdge,
                    Some(c) => ColumnType::Column(c),
                });

        let address = self.low_pc();

        Some(SourceLocation {
            line,
            column,
            path,
            address,
        })
    }

    /// Resolve an attribute by looking through both the origin or abstract die entries.
    pub(crate) fn attribute(
        &self,
        attribute_name: gimli::DwAt,
    ) -> Option<debug_info::GimliAttribute> {
        let attribute = collapsed_attribute(
            &self.function_die,
            self.specification_die.as_ref(),
            attribute_name,
        );

        if attribute.is_some() {
            return attribute.cloned();
        }

        // For inlined function, the *abstract instance* has to be checked if we cannot find the
        // attribute on the *concrete instance*. The abstract instance my also be a reference to a specification.
        if let Some(abstract_die) = &self.abstract_die {
            let inlined_specification_die = debug_info.resolve_die_reference(
                gimli::DW_AT_specification,
                abstract_die,
                self.unit_info,
            );
            let inline_attribute = collapsed_attribute(
                abstract_die,
                inlined_specification_die.as_ref(),
                attribute_name,
            );

            if inline_attribute.is_some() {
                return inline_attribute.cloned();
            }
        }

        None
    }

    /// Try to retrieve the frame base for this function
    pub fn frame_base(
        &self,
        debug_info: &super::DebugInfo,
        memory: &mut dyn MemoryInterface,
        frame_info: StackFrameInfo,
    ) -> Result<Option<u64>, DebugError> {
        match self.unit_info.extract_location(
            debug_info,
            &self.function_die,
            &VariableLocation::Unknown,
            memory,
            frame_info,
        )? {
            ExpressionResult::Location(VariableLocation::Address(address)) => Ok(Some(address)),
            ExpressionResult::Location(VariableLocation::RegisterValue(value)) => {
                Ok(value.try_into().ok())
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn parent_offset(&self) -> Option<UnitOffset> {
        self.unit_info.parent_offset(self.spec_offset())
    }

    pub(crate) fn spec_offset(&self) -> UnitOffset {
        self.specification_die
            .as_ref()
            .map(|d| d.offset())
            .unwrap_or(self.function_die.offset())
    }
}

// Try to retrieve the attribute from the specification or the function DIE.
fn collapsed_attribute<'a>(
    function_die: &'a Die,
    specification_die: Option<&'a Die>,
    attribute_name: gimli::DwAt,
) -> Option<&'a debug_info::GimliAttribute> {
    specification_die
        .as_ref()
        .and_then(|specification_die| specification_die.attr(attribute_name))
        .or_else(|| function_die.attr(attribute_name))
}
