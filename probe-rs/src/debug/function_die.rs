use std::ops::Range;

use crate::{debug::stack_frame::StackFrameInfo, MemoryInterface};

use super::{
    debug_info, extract_file,
    unit_info::{ExpressionResult, UnitInfo},
    ColumnType, DebugError, DebugInfo, SourceLocation, VariableLocation,
};

pub(crate) type Die<'abbrev, 'unit> =
    gimli::DebuggingInformationEntry<'abbrev, 'unit, debug_info::GimliReader, usize>;

/// Reference to a DIE for a function
#[derive(Clone)]
pub(crate) struct FunctionDie<'abbrev, 'unit> {
    /// A reference to the compilation unit this function belongs to.
    pub(crate) unit_info: &'unit UnitInfo,
    /// The DIE (Debugging Information Entry) for the function.
    pub(crate) function_die: Die<'abbrev, 'unit>,
    /// The optional specification DIE for the function, if it has one.
    /// - For regular functions, this applies to the `function_die`.
    /// - For inlined functions, this applies to the `abstract_die`.
    /// The specification DIE will contain separately declared attributes,
    /// e.g. for the function name.
    /// See DWARF spec, 2.13.2.
    pub(crate) specification_die: Option<Die<'abbrev, 'unit>>,
    /// Only present for inlined functions, where this is a reference
    /// to the declaration of the function.
    pub(crate) abstract_die: Option<Die<'abbrev, 'unit>>,
    /// The address ranges for which this function is valid.
    pub(crate) ranges: Vec<Range<u64>>,
}

impl<'abbrev, 'unit> FunctionDie<'abbrev, 'unit> {
    /// Create a new function DIE reference.
    /// We only return DIE's that are functions, with valid address ranges that represent machine code
    /// relevant to the address/program counter specified.
    /// Other DIE's will return None, and should be ignored.
    pub(crate) fn new(
        function_die: Die<'abbrev, 'unit>,
        unit_info: &'unit UnitInfo,
        debug_info: &'abbrev DebugInfo,
        address: u64,
    ) -> Result<Option<Self>, DebugError>
    where
        'abbrev: 'unit,
        'unit: 'abbrev,
    {
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
                //TODO: The DW_AT_subprograms with low_pc == 0 cause overlapping ranges with other 'valid' function dies, and obscures the correct function die.
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

    /// Test whether the given address is contained in the address ranges of this function.
    /// Use this, instead of checking for values between `low_pc()` and `high_pc()`, because
    /// the address ranges can be disjointed.
    pub(crate) fn range_contains(&self, address: u64) -> bool {
        self.ranges.iter().any(|range| range.contains(&address))
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
        let Some(fn_name_attr) = self.attribute(debug_info, gimli::DW_AT_name) else {
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

        let file_name_attr = self.attribute(debug_info, gimli::DW_AT_call_file)?;

        let (directory, file) =
            extract_file(debug_info, &self.unit_info.unit, file_name_attr.value())?;
        let line = self
            .attribute(debug_info, gimli::DW_AT_call_line)
            .and_then(|line| line.udata_value());

        let column =
            self.attribute(debug_info, gimli::DW_AT_call_column)
                .map(|column| match column.udata_value() {
                    None => ColumnType::LeftEdge,
                    Some(c) => ColumnType::Column(c),
                });
        Some(SourceLocation {
            line,
            column,
            file: Some(file),
            directory: Some(directory),
        })
    }

    /// Resolve an attribute by looking through both the specification and die, or abstract specification and die, entries.
    pub(crate) fn attribute(
        &self,
        debug_info: &super::DebugInfo,
        attribute_name: gimli::DwAt,
    ) -> Option<debug_info::GimliAttribute> {
        let attribute =
            collapsed_attribute(&self.function_die, &self.specification_die, attribute_name);

        if attribute.is_some() {
            return attribute;
        }

        // For inlined function, the *abstract instance* has to be checked if we cannot find the
        // attribute on the *concrete instance*. The abstract instance my also be a reference to a specification.
        if let Some(abstract_die) = &self.abstract_die {
            let inlined_specification_die = debug_info.resolve_die_reference(
                gimli::DW_AT_specification,
                abstract_die,
                self.unit_info,
            );
            let inline_attribute =
                collapsed_attribute(abstract_die, &inlined_specification_die, attribute_name);

            if inline_attribute.is_some() {
                return inline_attribute;
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
            _ => Ok(None),
        }
    }
}

// Try to retrieve the attribute from the specification or the function DIE.
fn collapsed_attribute(
    function_die: &Die,
    specification_die: &Option<Die>,
    attribute_name: gimli::DwAt,
) -> Option<gimli::Attribute<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>> {
    let attribute = specification_die
        .as_ref()
        .and_then(|specification_die| {
            specification_die
                .attr(attribute_name)
                .map_or(None, |attribute| attribute)
        })
        .or_else(|| {
            function_die
                .attr(attribute_name)
                .map_or(None, |attribute| attribute)
        });
    attribute
}
