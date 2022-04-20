#![warn(missing_docs)]

//! Debugging support for probe-rs
//!
//! The `debug` module contains various debug functionality, which can be
//! used to implement a debugger based on `probe-rs`.

// Bad things happen to the VSCode debug extenison and debug_adapter if we panic at the wrong time.
#![warn(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

/// Debug information which is parsed from DWARF debugging information.
pub mod debug_info;
/// Target Register definitions.
pub mod registers;
/// The stack frame information used while unwinding the stack from a specific program counter.
pub mod stack_frame;
/// Stepping granularity for stepping through a program during debug.
pub mod stepping_mode;
/// Variable information used during debug.
pub mod variable;
/// The hierarchical cache of all variables for a given scope.
pub mod variable_cache;

pub use self::{registers::*, stack_frame::StackFrame, variable::*, variable_cache::VariableCache};
use crate::{core::Core, MemoryInterface};
use gimli::{DebuggingInformationEntry, Location, UnitOffset};
use num_traits::Zero;
use std::{
    io,
    path::PathBuf,
    str::Utf8Error,
    sync::atomic::{AtomicI64, Ordering},
    vec,
};

/// An error occurred while debugging the target.
#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    /// An IO error occurred when accessing debug data.
    #[error("IO Error while accessing debug data")]
    Io(#[from] io::Error),
    /// An error occurred while accessing debug data.
    #[error("Error accessing debug data")]
    DebugData(#[from] object::read::Error),
    /// Something failed while parsing debug data.
    #[error("Error parsing debug data")]
    Parse(#[from] gimli::read::Error),
    /// Non-UTF8 data was found in the debug data.
    #[error("Non-UTF8 data found in debug data")]
    NonUtf8(#[from] Utf8Error),
    /// A probe-rs error occurred.
    #[error("Error using the probe")]
    Probe(#[from] crate::Error),
    /// A char could not be created from the given string.
    #[error(transparent)]
    CharConversion(#[from] std::char::CharTryFromError),
    /// An int could not be created from the given string.
    #[error(transparent)]
    IntConversion(#[from] std::num::TryFromIntError),
    /// Errors encountered while determining valid halt locations for breakpoints and stepping.
    /// These are distinct from other errors because they terminate the current step, and result in a user message, but they do not interrupt the rest of the debug session.
    #[error("{message}  @program_counter={:#010X}.", pc_at_error)]
    NoValidHaltLocation {
        /// A message that can be displayed to the user to help them make an informed recovery choice.
        message: String,
        /// The value of the program counter for which a halt was requested.
        pc_at_error: u64,
    },
    /// Some other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// A copy of [`gimli::ColumnType`] which uses [`u64`] instead of [`NonZeroU64`].
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ColumnType {
    /// The `LeftEdge` means that the statement begins at the start of the new line.
    LeftEdge,
    /// A column number, whose range begins at 1.
    Column(u64),
}

impl From<gimli::ColumnType> for ColumnType {
    fn from(column: gimli::ColumnType) -> Self {
        match column {
            gimli::ColumnType::LeftEdge => ColumnType::LeftEdge,
            gimli::ColumnType::Column(c) => ColumnType::Column(c.get()),
        }
    }
}

static CACHE_KEY: AtomicI64 = AtomicI64::new(1);
/// Generate a unique key that can be used to assign id's to StackFrame and Variable structs.
pub fn get_sequential_key() -> i64 {
    CACHE_KEY.fetch_add(1, Ordering::SeqCst)
}

/// A specific location in source code.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceLocation {
    /// The line number in the source file with zero based indexing.
    pub line: Option<u64>,
    /// The column number in the source file with zero based indexing.
    pub column: Option<ColumnType>,
    /// The file name of the source file.
    pub file: Option<String>,
    /// The directory of the source file.
    pub directory: Option<PathBuf>,
    /// The address of the first instruction associated with the source code
    pub low_pc: Option<u32>,
    /// The address of the first location past the last instruction associated with the source code
    pub high_pc: Option<u32>,
}

type FunctionDieType<'abbrev, 'unit> =
    gimli::DebuggingInformationEntry<'abbrev, 'unit, debug_info::GimliReader, usize>;

type UnitIter =
    gimli::DebugInfoUnitHeadersIter<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;

/// Reference to a DIE for a function
pub(crate) struct FunctionDie<'abbrev, 'unit, 'unit_info, 'debug_info> {
    unit_info: &'unit_info UnitInfo<'debug_info>,

    function_die: FunctionDieType<'abbrev, 'unit>,

    /// Only present for inlined functions, where this is a reference
    /// to the declaration of the function.
    abstract_die: Option<FunctionDieType<'abbrev, 'unit>>,
    /// The address of the first instruction in this function.
    low_pc: u64,
    /// The address of the first instruction after this funciton.
    high_pc: u64,
}

impl<'debugunit, 'abbrev, 'unit: 'debugunit, 'unit_info, 'debug_info>
    FunctionDie<'abbrev, 'unit, 'unit_info, 'debug_info>
{
    fn new(
        die: FunctionDieType<'abbrev, 'unit>,
        unit_info: &'unit_info UnitInfo<'debug_info>,
    ) -> Option<Self> {
        let tag = die.tag();

        match tag {
            gimli::DW_TAG_subprogram => Some(Self {
                unit_info,
                function_die: die,
                abstract_die: None,
                low_pc: 0,
                high_pc: 0,
            }),
            other_tag => {
                log::error!("FunctionDie has to has to have Tag DW_TAG_subprogram, but tag is {:?}. This is a bug, please report it.", other_tag.static_string());
                None
            }
        }
    }

    fn new_inlined(
        concrete_die: FunctionDieType<'abbrev, 'unit>,
        abstract_die: FunctionDieType<'abbrev, 'unit>,
        unit_info: &'unit_info UnitInfo<'debug_info>,
    ) -> Option<Self> {
        let tag = concrete_die.tag();

        match tag {
            gimli::DW_TAG_inlined_subroutine => Some(Self {
                unit_info,
                function_die: concrete_die,
                abstract_die: Some(abstract_die),
                low_pc: 0,
                high_pc: 0,
            }),
            other_tag => {
                log::error!("FunctionDie has to has to have Tag DW_TAG_inlined_subroutine, but tag is {:?}. This is a bug, please report it.", other_tag.static_string());
                None
            }
        }
    }

    fn is_inline(&self) -> bool {
        self.abstract_die.is_some()
    }

    fn function_name(&self) -> Option<String> {
        if let Some(fn_name_attr) = self.get_attribute(gimli::DW_AT_name) {
            match fn_name_attr.value() {
                gimli::AttributeValue::DebugStrRef(fn_name_ref) => {
                    match self.unit_info.debug_info.dwarf.string(fn_name_ref) {
                        Ok(fn_name_raw) => Some(String::from_utf8_lossy(&fn_name_raw).to_string()),
                        Err(error) => {
                            log::debug!("No value for DW_AT_name: {:?}: error", error);

                            None
                        }
                    }
                }
                value => {
                    log::debug!("Unexpected attribute value for DW_AT_name: {:?}", value);
                    None
                }
            }
        } else {
            log::debug!("DW_AT_name attribute not found, unable to retrieve function name");
            None
        }
    }

    /// Get the call site of an inlined function.
    ///
    /// If this function is not inlined (`is_inline()` returns false),
    /// this function returns `None`.
    fn inline_call_location(&self) -> Option<SourceLocation> {
        if !self.is_inline() {
            return None;
        }

        let file_name_attr = self.get_attribute(gimli::DW_AT_call_file)?;

        let (directory, file) = extract_file(
            self.unit_info.debug_info,
            &self.unit_info.unit,
            file_name_attr.value(),
        )?;
        let line = self
            .get_attribute(gimli::DW_AT_call_line)
            .and_then(|line| line.udata_value());

        let column =
            self.get_attribute(gimli::DW_AT_call_column)
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
    fn get_attribute(&self, attribute_name: gimli::DwAt) -> Option<debug_info::GimliAttribute> {
        let attribute = self
            .function_die
            .attr(attribute_name)
            .map_or(None, |attribute| attribute);

        // For inlined function, the *abstract instance* has to be checked if we cannot find the
        // attribute on the *concrete instance*.
        if self.is_inline() && attribute.is_none() {
            if let Some(origin) = self.abstract_die.as_ref() {
                origin
                    .attr(attribute_name)
                    .map_or(None, |attribute| attribute)
            } else {
                None
            }
        } else {
            attribute
        }
    }
}

pub(crate) struct UnitInfo<'debuginfo> {
    debug_info: &'debuginfo debug_info::DebugInfo,
    unit: gimli::Unit<debug_info::GimliReader, usize>,
}

impl<'debuginfo> UnitInfo<'debuginfo> {
    /// Get the DIE for the function containing the given address.
    ///
    /// If `find_inlined` is `false`, then the result will contain a single [`FunctionDie`]
    fn get_function_dies(
        &self,
        address: u64,
        find_inlined: bool,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        log::trace!("Searching Function DIE for address {:#010x}", address);

        let mut entries_cursor = self.unit.entries();

        while let Ok(Some((_depth, current))) = entries_cursor.next_dfs() {
            if current.tag() == gimli::DW_TAG_subprogram {
                let mut ranges = self.debug_info.dwarf.die_ranges(&self.unit, current)?;

                while let Ok(Some(ranges)) = ranges.next() {
                    if (ranges.begin <= address) && (address < ranges.end) {
                        // Check if we are actually in an inlined function

                        if let Some(mut die) = FunctionDie::new(current.clone(), self) {
                            die.low_pc = ranges.begin;
                            die.high_pc = ranges.end;

                            let mut functions = vec![die];

                            if find_inlined {
                                log::debug!(
                                    "Found DIE, now checking for inlined functions: name={:?}",
                                    functions[0].function_name()
                                );

                                let inlined_functions =
                                    self.find_inlined_functions(address, current.offset())?;

                                if inlined_functions.is_empty() {
                                    log::debug!("No inlined function found!");
                                } else {
                                    log::debug!(
                                        "{} inlined functions for address {:#010x}",
                                        inlined_functions.len(),
                                        address
                                    );
                                    functions.extend(inlined_functions.into_iter());
                                }

                                return Ok(functions);
                            } else {
                                log::debug!("Found DIE: name={:?}", functions[0].function_name());
                            }
                            return Ok(functions);
                        }
                    }
                }
            }
        }
        Ok(vec![])
    }

    /// Check if the function located at the given offset contains inlined functions at the
    /// given address.
    fn find_inlined_functions(
        &self,
        address: u64,
        offset: UnitOffset,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        let mut current_depth = 0;

        let mut abort_depth = 0;

        let mut functions = Vec::new();

        if let Ok(mut cursor) = self.unit.entries_at_offset(offset) {
            while let Ok(Some((depth, current))) = cursor.next_dfs() {
                current_depth += depth;

                if current_depth < abort_depth {
                    break;
                }

                if current.tag() == gimli::DW_TAG_inlined_subroutine {
                    let mut ranges = self.debug_info.dwarf.die_ranges(&self.unit, current)?;

                    while let Ok(Some(ranges)) = ranges.next() {
                        if (ranges.begin <= address) && (address < ranges.end) {
                            // Check if we are actually in an inlined function

                            // We don't have to search further up in the tree, if there are multiple inlined functions,
                            // they will be children of the current function.
                            abort_depth = current_depth;

                            // Find the abstract definition
                            if let Ok(Some(abstract_origin)) =
                                current.attr(gimli::DW_AT_abstract_origin)
                            {
                                match abstract_origin.value() {
                                    gimli::AttributeValue::UnitRef(unit_ref) => {
                                        if let Ok(abstract_die) = self.unit.entry(unit_ref) {
                                            if let Some(mut die) = FunctionDie::new_inlined(
                                                current.clone(),
                                                abstract_die.clone(),
                                                self,
                                            ) {
                                                die.low_pc = ranges.begin;
                                                die.high_pc = ranges.end;

                                                functions.push(die);
                                            }
                                        }
                                    }
                                    other_value => log::warn!(
                                        "Unsupported DW_AT_abstract_origin value: {:?}",
                                        other_value
                                    ),
                                }
                            } else {
                                log::warn!("No abstract origin for inlined function, skipping.");
                                return Ok(vec![]);
                            }
                        }
                    }
                }
            }
        }

        Ok(functions)
    }

    /// Recurse the ELF structure below the `tree_node`, and ...
    /// - Consumes the `child_variable`.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    fn process_tree_node_attributes(
        &self,
        tree_node: &mut gimli::EntriesTreeNode<debug_info::GimliReader>,
        parent_variable: &mut Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &registers::Registers,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        // Identify the parent.
        child_variable.parent_key = Some(parent_variable.variable_key);

        // It often happens that intermediate nodes exist for structure reasons,
        // so we need to pass values like 'member_index' from the parent down to the next level child nodes.

        // TODO: This does not work for arrays of structs. Figure where / if this is necessary.

        //if parent_variable.member_index.is_some() {
        //    child_variable.member_index = parent_variable.member_index;
        //}

        // We need to determine if we are working with a 'abstract` location, and use that node for the attributes we need
        // let mut origin_tree:Option<gimli::EntriesTree<GimliReader<>>> = None;
        let attributes_entry = if let Ok(Some(abstract_origin)) =
            tree_node.entry().attr(gimli::DW_AT_abstract_origin)
        {
            match abstract_origin.value() {
                gimli::AttributeValue::UnitRef(unit_ref) => Some(
                    self.unit
                        .header
                        .entries_tree(&self.unit.abbreviations, Some(unit_ref))?
                        .root()?
                        .entry()
                        .clone(),
                ),
                other_attribute_value => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "Unimplemented: Attribute Value for DW_AT_abstract_origin {:?}",
                        other_attribute_value
                    )));
                    None
                }
            }
        } else {
            Some(tree_node.entry().clone())
        };

        // Try to exact the name first, for easier debugging

        if let Some(name) = attributes_entry
            .as_ref()
            .map(|ae| ae.attr_value(gimli::DW_AT_name))
            .transpose()?
            .flatten()
        {
            child_variable.name = VariableName::Named(extract_name(self.debug_info, name));
        }

        // For variable attribute resolution, we need to resolve a few attributes in advance of looping through all the other ones.

        // We need to process the location attribute to ensure that location is known before we calculate type.
        child_variable = self.extract_location(
            tree_node,
            parent_variable,
            child_variable,
            core,
            stack_frame_registers,
            cache,
        )?;

        if let Some(attributes_entry) = attributes_entry {
            let mut variable_attributes = attributes_entry.attrs();

            // Now loop through all the unit attributes to extract the remainder of the `Variable` definition.
            while let Ok(Some(attr)) = variable_attributes.next() {
                match attr.name() {
                    gimli::DW_AT_location | gimli::DW_AT_data_member_location => {
                        // The child_variable.location is calculated with attribute gimli::DW_AT_type, to ensure it gets done before DW_AT_type is processed
                    }
                    gimli::DW_AT_name => {
                        child_variable.name =
                            VariableName::Named(extract_name(self.debug_info, attr.value()));
                    }
                    gimli::DW_AT_decl_file => {
                        if let Some((directory, file_name)) =
                            extract_file(self.debug_info, &self.unit, attr.value())
                        {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: existing_source_location.line,
                                        column: existing_source_location.column,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: None,
                                        column: None,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                            }
                        };
                    }
                    gimli::DW_AT_decl_line => {
                        if let Some(line_number) = extract_line(attr.value()) {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: existing_source_location.column,
                                        file: existing_source_location.file,
                                        directory: existing_source_location.directory,
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: None,
                                        file: None,
                                        directory: None,
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                            }
                        };
                    }
                    gimli::DW_AT_decl_column => {
                        // Unused.
                    }
                    gimli::DW_AT_containing_type => {
                        // TODO: Implement [documented RUST extensions to DWARF standard](https://rustc-dev-guide.rust-lang.org/debugging-support-in-rustc.html?highlight=dwarf#dwarf-and-rustc)
                    }
                    gimli::DW_AT_type => {
                        match attr.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                // Reference to a type, or an entry to another type or a type modifier which will point to another type.
                                let mut type_tree = self
                                    .unit
                                    .header
                                    .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                                let tree_node = type_tree.root()?;
                                child_variable = self.extract_type(
                                    tree_node,
                                    parent_variable,
                                    child_variable,
                                    core,
                                    stack_frame_registers,
                                    cache,
                                )?;
                            }
                            other_attribute_value => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_type {:?}",
                                    other_attribute_value
                                )));
                            }
                        }
                    }
                    gimli::DW_AT_enum_class => match attr.value() {
                        gimli::AttributeValue::Flag(is_enum_class) => {
                            if is_enum_class {
                                child_variable.set_value(VariableValue::Valid(format!(
                                    "{:?}",
                                    child_variable.type_name.clone()
                                )));
                            } else {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Flag Value for DW_AT_enum_class {:?}",
                                    is_enum_class
                                )));
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_enum_class: {:?}",
                                other_attribute_value
                            )));
                        }
                    },
                    gimli::DW_AT_const_value => match attr.value() {
                        gimli::AttributeValue::Udata(const_value) => {
                            child_variable.set_value(VariableValue::Valid(const_value.to_string()));
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_const_value: {:?}",
                                other_attribute_value
                            )));
                        }
                    },
                    gimli::DW_AT_alignment => {
                        // TODO: Figure out when (if at all) we need to do anything with DW_AT_alignment for the purposes of decoding data values.
                    }
                    gimli::DW_AT_artificial => {
                        // These are references for entries like discriminant values of `VariantParts`.
                        child_variable.name = VariableName::Artifical;
                    }
                    gimli::DW_AT_discr => match attr.value() {
                        // This calculates the active discriminant value for the `VariantPart`.
                        gimli::AttributeValue::UnitRef(unit_ref) => {
                            let mut type_tree = self
                                .unit
                                .header
                                .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                            let mut discriminant_node = type_tree.root()?;
                            let mut discriminant_variable = cache.cache_variable(
                                Some(parent_variable.variable_key),
                                Variable::new(
                                    self.unit.header.offset().as_debug_info_offset(),
                                    Some(discriminant_node.entry().offset()),
                                ),
                                core,
                            )?;
                            discriminant_variable = self.process_tree_node_attributes(
                                &mut discriminant_node,
                                parent_variable,
                                discriminant_variable,
                                core,
                                stack_frame_registers,
                                cache,
                            )?;
                            if !discriminant_variable.is_valid() {
                                parent_variable.role = VariantRole::VariantPart(u64::MAX);
                            } else {
                                parent_variable.role = VariantRole::VariantPart(
                                    discriminant_variable
                                        .get_value(cache)
                                        .parse()
                                        .unwrap_or(u64::MAX)
                                        as u64,
                                );
                            }
                            cache.remove_cache_entry(discriminant_variable.variable_key)?;
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_discr {:?}",
                                other_attribute_value
                            )));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_lower_bound => match attr.value().udata_value() {
                        Some(lower_bound) => child_variable.range_lower_bound = lower_bound as i64,
                        None => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_lower_bound: {:?}",
                                attr.value()
                            )));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_upper_bound | gimli::DW_AT_count => {
                        match attr.value().udata_value() {
                            Some(upper_bound) => {
                                child_variable.range_upper_bound = upper_bound as i64
                            }
                            None => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_upper_bound: {:?}",
                                    attr.value()
                                )));
                            }
                        }
                    }
                    gimli::DW_AT_external => {
                        // TODO: Implement globally visible variables.
                    }
                    gimli::DW_AT_declaration => {
                        // Unimplemented.
                    }
                    gimli::DW_AT_encoding => {
                        // Ignore these. RUST data types handle this intrinsicly.
                    }
                    gimli::DW_AT_discr_value => {
                        // Processed by `extract_variant_discriminant()`.
                    }
                    gimli::DW_AT_byte_size => {
                        // Processed by `extract_byte_size()`.
                    }
                    gimli::DW_AT_abstract_origin => {
                        // Processed before looping through all attributes
                    }
                    gimli::DW_AT_linkage_name => {
                        // Unused attribute of, for example, inlined DW_TAG_subroutine
                    }
                    gimli::DW_AT_address_class => {
                        // Processed by `extract_type()`
                    }
                    other_attribute => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Unimplemented: Variable Attribute {:?} : {:?}, with children = {}",
                            other_attribute.static_string(),
                            tree_node.entry().attr_value(other_attribute),
                            tree_node.entry().has_children()
                        )));
                    }
                }
            }
        }
        cache
            .cache_variable(child_variable.parent_key, child_variable, core)
            .map_err(|error| error.into())
    }

    /// Recurse the ELF structure below the `parent_node`, and ...
    /// - Consumes the `parent_variable`.
    /// - Updates the `DebugInfo::VariableCache` with all descendant `Variable`s.
    /// - Returns a clone of the most up-to-date `parent_variable` in the cache.
    fn process_tree(
        &self,
        parent_node: gimli::EntriesTreeNode<debug_info::GimliReader>,
        mut parent_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &registers::Registers,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        if parent_variable.is_valid() {
            let program_counter =
                if let Some(program_counter) = stack_frame_registers.get_program_counter() {
                    u64::from(program_counter)
                } else {
                    return Err(DebugError::Other(anyhow::anyhow!(
                        "Cannot unwind `Variable` without a valid PC (program_counter)"
                    )));
                };

            log::trace!("process_tree for parent {}", parent_variable.variable_key);

            let mut child_nodes = parent_node.children();
            while let Some(mut child_node) = child_nodes.next()? {
                match child_node.entry().tag() {
                    gimli::DW_TAG_namespace => {
                        // Use these parents to extract `statics`.
                        let mut namespace_variable = Variable::new(
                            self.unit.header.offset().as_debug_info_offset(),
                            Some(child_node.entry().offset()),
                        );

                        namespace_variable.name = if let Ok(Some(attr)) = child_node.entry().attr(gimli::DW_AT_name) {
                            VariableName::Namespace(extract_name(self.debug_info, attr.value()))
                        } else { VariableName::AnonymousNamespace };
                        namespace_variable.type_name = VariableType::Namespace;
                        namespace_variable.memory_location = variable::VariableLocation::Unavailable;
                        namespace_variable = cache.cache_variable(Some(parent_variable.variable_key), namespace_variable, core)?;

                        let mut namespace_children_nodes = child_node.children();
                        while let Some(mut namespace_child_node) = namespace_children_nodes.next()? {
                            match namespace_child_node.entry().tag() {
                                gimli::DW_TAG_variable => {
                                    // We only want the TOP level variables of the namespace (statics).
                                    let static_child_variable = cache.cache_variable(Some(namespace_variable.variable_key), Variable::new(
                                        self.unit.header.offset().as_debug_info_offset(),
                                        Some(namespace_child_node.entry().offset()),), core)?;
                                    self.process_tree_node_attributes(&mut namespace_child_node, &mut namespace_variable, static_child_variable, core, stack_frame_registers, cache)?;
                                }
                                gimli::DW_TAG_namespace => {
                                    // Recurse for additional namespace variables.
                                    let mut namespace_child_variable = Variable::new(
                                        self.unit.header.offset().as_debug_info_offset(),
                                        Some(namespace_child_node.entry().offset()),);
                                    namespace_child_variable.name = if let Ok(Some(attr)) = namespace_child_node.entry().attr(gimli::DW_AT_name) {

                                        match &namespace_variable.name {
                                            VariableName::Namespace(name) => {
                                            VariableName::Namespace(format!("{}::{}", name, extract_name(self.debug_info, attr.value())))
                                            }
                                            other => return Err(DebugError::Other(anyhow::anyhow!("Unable to construct namespace variable, unexpected parent name: {:?}", other)))
                                        }

                                    } else { VariableName::AnonymousNamespace};
                                    namespace_child_variable.type_name = VariableType::Namespace;
                                    namespace_child_variable.memory_location = variable::VariableLocation::Unavailable;
                                    namespace_child_variable = cache.cache_variable(Some(namespace_variable.variable_key), namespace_child_variable, core)?;
                                    namespace_child_variable = self.process_tree(namespace_child_node, namespace_child_variable, core, stack_frame_registers, cache, )?;
                                    if !cache.has_children(&namespace_child_variable)? {
                                        cache.remove_cache_entry(namespace_child_variable.variable_key)?;
                                    }
                                }
                                _ => {
                                    // We only want namespace and variable children.
                                }
                            }
                        }
                        if !cache.has_children(&namespace_variable)? {
                            cache.remove_cache_entry(namespace_variable.variable_key)?;
                        }
                    }
                    gimli::DW_TAG_variable |    // Typical top-level variables.
                    gimli::DW_TAG_member |      // Members of structured types.
                    gimli::DW_TAG_enumerator    // Possible values for enumerators, used by extract_type() when processing DW_TAG_enumeration_type.
                    => {
                        let mut child_variable = cache.cache_variable(Some(parent_variable.variable_key), Variable::new(
                        self.unit.header.offset().as_debug_info_offset(),
                        Some(child_node.entry().offset()),
                    ), core)?;
                        child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers, cache,)?;

                        // Do not keep or process PhantomData nodes, or variant parts that we have already used.
                        if child_variable.type_name.is_phantom_data()
                            ||  child_variable.name == VariableName::Artifical
                        {
                            cache.remove_cache_entry(child_variable.variable_key)?;
                        } else if child_variable.is_valid() {
                            // Recursively process each child.
                            self.process_tree(child_node, child_variable, core, stack_frame_registers, cache, )?;
                        }
                    }
                    gimli::DW_TAG_variant_part => {
                        // We need to recurse through the children, to find the DW_TAG_variant with discriminant matching the DW_TAG_variant, 
                        // and ONLY add it's children to the parent variable. 
                        // The structure looks like this (there are other nodes in the structure that we use and discard before we get here):
                        // Level 1: --> An actual variable that has a variant value
                        //      Level 2: --> this DW_TAG_variant_part node (some child nodes are used to calc the active Variant discriminant)
                        //          Level 3: --> Some DW_TAG_variant's that have discriminant values to be matched against the discriminant 
                        //              Level 4: --> The actual variables, with matching discriminant, which will be added to `parent_variable`
                        // TODO: Handle Level 3 nodes that belong to a DW_AT_discr_list, instead of having a discreet DW_AT_discr_value 
                        let mut child_variable = cache.cache_variable(
                            Some(parent_variable.variable_key),
                            Variable::new(self.unit.header.offset().as_debug_info_offset(),Some(child_node.entry().offset())),
                            core
                        )?;
                        // To determine the discriminant, we use the following rules:
                        // - If there is no DW_AT_discr, then there will be a single DW_TAG_variant, and this will be the matching value. In the code here, we assign a default value of u64::MAX to both, so that they will be matched as belonging together (https://dwarfstd.org/ShowIssue.php?issue=180517.2)
                        // - TODO: The [DWARF] standard, 5.7.10, allows for a case where there is no DW_AT_discr attribute, but a DW_AT_type to represent the tag. I have not seen that generated from RUST yet.
                        // - If there is a DW_AT_discr that has a value, then this is a reference to the member entry for the discriminant. This value will be resolved to match against the appropriate DW_TAG_variant.
                        // - TODO: The [DWARF] standard, 5.7.10, allows for a DW_AT_discr_list, but I have not seen that generated from RUST yet. 
                        parent_variable.role = VariantRole::VariantPart(u64::MAX);
                        child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers, cache, )?;
                        // At this point we have everything we need (It has updated the parent's `role`) from the child_variable, so elimnate it before we continue ...
                        cache.remove_cache_entry(child_variable.variable_key)?;
                        parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers, cache)?;
                    }
                    gimli::DW_TAG_variant // variant is a child of a structure, and one of them should have a discriminant value to match the DW_TAG_variant_part 
                    => {
                        // We only need to do this if we have not already found our variant,
                        if !cache.has_children(&parent_variable)? {
                            let mut child_variable = cache.cache_variable(
                                Some(parent_variable.variable_key),
                                Variable::new(self.unit.header.offset().as_debug_info_offset(), Some(child_node.entry().offset())),
                                core
                            )?;
                            self.extract_variant_discriminant(&child_node, &mut child_variable)?;
                            child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers, cache)?;
                            if child_variable.is_valid() {
                                if let VariantRole::Variant(discriminant) = child_variable.role {
                                    // Only process the discriminant variants or when we eventually   encounter the default 
                                    if parent_variable.role == VariantRole::VariantPart(discriminant) || discriminant == u64::MAX {
                                        // Pass some key values through intermediate nodes to valid desccendants.
                                        child_variable.memory_location = parent_variable.memory_location.clone();
                                        // Recursively process each relevant child node.
                                        child_variable = self.process_tree(child_node, child_variable, core, stack_frame_registers, cache)?;
                                        if child_variable.is_valid() {
                                            // Eliminate intermediate DWARF nodes, but keep their children
                                            cache.adopt_grand_children(&parent_variable, &child_variable)?;
                                        }
                                    } else {
                                        cache.remove_cache_entry(child_variable.variable_key)?;
                                    }
                                }
                            } else {
                                cache.remove_cache_entry(child_variable.variable_key)?;
                            }
                        }
                    }
                    gimli::DW_TAG_subrange_type => {
                        // This tag is a child node fore parent types such as (array, vector, etc.).
                        // Recursively process each node, but pass the parent_variable so that new children are caught despite missing these tags.
                        let mut range_variable = cache.cache_variable(Some(parent_variable.variable_key),Variable::new(
                        self.unit.header.offset().as_debug_info_offset(),
                        Some(child_node.entry().offset()),
                    ), core)?;
                        range_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, range_variable, core, stack_frame_registers, cache)?;
                        // Determine if we should use the results ...
                        if range_variable.is_valid() {
                            // Pass the pertinent info up to the parent_variable.
                            parent_variable.type_name = range_variable.type_name;
                            parent_variable.range_lower_bound = range_variable.range_lower_bound;
                            parent_variable.range_upper_bound = range_variable.range_upper_bound;
                        }
                        cache.remove_cache_entry(range_variable.variable_key)?;
                    }
                    gimli::DW_TAG_lexical_block => {
                        // Determine the low and high ranges for which this DIE and children are in scope. These can be specified discreetly, or in ranges. 
                        let mut in_scope =  false;
                        if let Ok(Some(low_pc_attr)) = child_node.entry().attr(gimli::DW_AT_low_pc) {
                            let low_pc = match low_pc_attr.value() {
                                gimli::AttributeValue::Addr(value) => value as u64,
                                _other => u64::MAX,
                            };
                            let high_pc = if let Ok(Some(high_pc_attr))
                                = child_node.entry().attr(gimli::DW_AT_high_pc) {
                                    match high_pc_attr.value() {
                                        gimli::AttributeValue::Addr(addr) => addr,
                                        gimli::AttributeValue::Udata(unsigned_offset) => low_pc + unsigned_offset,
                                        _other => 0_u64,
                                    }
                            } else { 0_u64};
                            if low_pc == u64::MAX || high_pc == 0_u64 {
                                // These have not been specified correctly ... something went wrong.
                                parent_variable.set_value(VariableValue::Error("Error: Processing of variables failed because of invalid/unsupported scope information. Please log a bug at 'https://github.com/probe-rs/probe-rs/issues'".to_string()));
                            }
                            if low_pc <= program_counter && program_counter < high_pc {
                                // We have established positive scope, so no need to continue.
                                in_scope = true;
                            };
                            // No scope info yet, so keep looking. 
                        };
                        // Searching for ranges has a bit more overhead, so ONLY do this if do not have scope confirmed yet.
                        if !in_scope {
                            if let Ok(Some(ranges))
                                = child_node.entry().attr(gimli::DW_AT_ranges) {
                                    match ranges.value() {
                                        gimli::AttributeValue::RangeListsRef(raw_range_lists_offset) => {
                                            let range_lists_offset = self.debug_info.dwarf.ranges_offset_from_raw(&self.unit, raw_range_lists_offset);

                                            if let Ok(mut ranges) = self
                                                .debug_info
                                                .dwarf
                                                .ranges(&self.unit, range_lists_offset) {
                                                    while let Ok(Some(ranges)) = ranges.next() {
                                                        // We have established positive scope, so no need to continue.
                                                        if ranges.begin <= program_counter && program_counter < ranges.end {
                                                            in_scope = true;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        other_range_attribute => {
                                            parent_variable.set_value(VariableValue::Error(format!("Found unexpected scope attribute: {:?} for variable {:?}", other_range_attribute, parent_variable.name)));
                                        }
                                    }
                            }
                        }
                        if in_scope {
                            // This is IN scope.
                            // Recursively process each child, but pass the parent_variable, so that we don't create intermediate nodes for scope identifiers.
                            parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers, cache)?;
                        } else {
                            parent_variable.set_value(VariableValue::Error("<lexical block no longer in scope>".to_string()));
                        }
                    }
                    gimli::DW_TAG_template_type_parameter => {
                        // The parent node for Rust generic type parameter
                        // These show up as a child of structures they belong to and points to the type that matches the template.
                        // They are followed by a sibling of `DW_TAG_member` with name '__0' that has all the attributes needed to resolve the value.
                        // TODO: If there are multiple types supported, then I suspect there will be additional `DW_TAG_member` siblings. We will need to match those correctly.
                    }
                    other => {
                        // One of two things are true here. Either we've encountered a DwTag that is implemented in `extract_type`, and whould be ignored, or we have encountered an unimplemented  DwTag.
                        match other {
                            gimli::DW_TAG_formal_parameter | // Parameters to functions are not included in our processing of variables.
                            gimli::DW_TAG_inlined_subroutine | // Inlined subroutines are handled at the [StackFame] level
                            gimli::DW_TAG_base_type |
                            gimli::DW_TAG_pointer_type |
                            gimli::DW_TAG_structure_type |
                            gimli::DW_TAG_enumeration_type |
                            gimli::DW_TAG_array_type |
                            gimli::DW_TAG_subroutine_type |
                            gimli::DW_TAG_subprogram |
                            gimli::DW_TAG_union_type => {
                                // These will be processed elsewhere, or not at all, until we discover a use case that needs to be implemented.
                            }
                            unimplemented => {
                                parent_variable.set_value(VariableValue::Error(format!("Unimplemented: Encountered unimplemented DwTag {:?} for Variable {:?}", unimplemented.static_string(), parent_variable)));
                            }
                        }
                    }
                }
            }
        }
        cache
            .cache_variable(parent_variable.parent_key, parent_variable, core)
            .map_err(|error| error.into())
    }

    /// Compute the discriminant value of a DW_TAG_variant variable. If it is not explicitly captured in the DWARF, then it is the default value.
    fn extract_variant_discriminant(
        &self,
        node: &gimli::EntriesTreeNode<debug_info::GimliReader>,
        variable: &mut Variable,
    ) -> Result<(), DebugError> {
        if node.entry().tag() == gimli::DW_TAG_variant {
            variable.role = match node.entry().attr(gimli::DW_AT_discr_value) {
                Ok(optional_discr_value_attr) => {
                    match optional_discr_value_attr {
                        Some(discr_attr) => {
                            match discr_attr.value() {
                                gimli::AttributeValue::Data1(const_value) => {
                                    VariantRole::Variant(const_value as u64)
                                }
                                other_attribute_value => {
                                    variable.set_value(VariableValue::Error(format!("Unimplemented: Attribute Value for DW_AT_discr_value: {:?}", other_attribute_value)));
                                    VariantRole::Variant(u64::MAX)
                                }
                            }
                        }
                        None => {
                            // In the case where the variable is a DW_TAG_variant, but has NO DW_AT_discr_value, then this is the "default" to be used.
                            VariantRole::Variant(u64::MAX)
                        }
                    }
                }
                Err(_error) => {
                    variable.set_value(VariableValue::Error(format!(
                        "Error: Retrieving DW_AT_discr_value for variable {:?}",
                        variable
                    )));
                    VariantRole::NonVariant
                }
            };
        }
        Ok(())
    }

    /// Compute the type (base to complex) of a variable. Only base types have values.
    /// Complex types are references to node trees, that require traversal in similar ways to other DIE's like functions.
    /// This means both [`get_function_variables()`] and [`extract_type()`] will call the recursive [`process_tree()`] method to build an integrated `tree` of variables with types and values.
    /// - Consumes the `child_variable`.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    fn extract_type(
        &self,
        node: gimli::EntriesTreeNode<debug_info::GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &registers::Registers,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        let type_name = match node.entry().attr(gimli::DW_AT_name) {
            Ok(optional_name_attr) => {
                optional_name_attr.map(|name_attr| extract_name(self.debug_info, name_attr.value()))
            }
            Err(error) => {
                let message = format!("Error: evaluating type name: {:?} ", error);
                child_variable.set_value(VariableValue::Error(message.clone()));
                Some(message)
            }
        };

        if child_variable.is_valid() {
            match &child_variable.type_name {
                VariableType::Struct(type_name)
                    if type_name.starts_with("&str")
                        || type_name.starts_with("Option")
                        || type_name.starts_with("Some")
                        || type_name.starts_with("Result")
                        || type_name.starts_with("Ok")
                        || type_name.starts_with("Err") =>
                {
                    // In some cases, it really simplifies the UX if we can auto resolve the children and derive a value that is visible at first glance to the user.
                    child_variable.variable_node_type = VariableNodeType::RecurseToBaseType;
                }
                VariableType::Pointer(Some(name))
                    if name.starts_with("*const") || name.starts_with("*mut") =>
                {
                    // In some cases, it really simplifies the UX if we can auto resolve the children and derive a value that is visible at first glance to the user.
                    child_variable.variable_node_type = VariableNodeType::RecurseToBaseType;
                }
                _ => (),
            }

            child_variable.byte_size = extract_byte_size(self.debug_info, node.entry());

            match node.entry().tag() {
                gimli::DW_TAG_base_type => {
                    if let Some(child_member_index) = child_variable.member_index {
                        match &parent_variable.memory_location {
                            variable::VariableLocation::Address(address) => {
                                // This is a member of an array type, and needs special handling.
                                let (location, has_overflowed) = address.overflowing_add(
                                    child_member_index as u32 * child_variable.byte_size as u32,
                                );

                                if has_overflowed {
                                    return Err(DebugError::Other(anyhow::anyhow!(
                                        "Overflow calculating variable address"
                                    )));
                                } else {
                                    child_variable.memory_location =
                                        variable::VariableLocation::Address(location);
                                }
                            }
                            _other => {
                                child_variable.memory_location =
                                    variable::VariableLocation::Unavailable;
                            }
                        }
                    }

                    child_variable.type_name =
                        VariableType::Base(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                }

                gimli::DW_TAG_pointer_type => {
                    child_variable.type_name = VariableType::Pointer(type_name);

                    // This needs to resolve the pointer before the regular recursion can continue.
                    match node.entry().attr(gimli::DW_AT_type) {
                        Ok(optional_data_type_attribute) => {
                            match optional_data_type_attribute {
                                Some(data_type_attribute) => {
                                    match data_type_attribute.value() {
                                        gimli::AttributeValue::UnitRef(unit_ref) => {
                                            if child_variable.variable_node_type
                                                == VariableNodeType::RecurseToBaseType
                                            {
                                                // Resolve the children of this variable, because they contain essential information required to resolve the value
                                                child_variable.variable_node_type =
                                                    VariableNodeType::ReferenceOffset(unit_ref);
                                                self.debug_info.cache_deferred_variables(
                                                    cache,
                                                    core,
                                                    &mut child_variable,
                                                    stack_frame_registers,
                                                )?;
                                            } else {
                                                child_variable.variable_node_type =
                                                    VariableNodeType::ReferenceOffset(unit_ref);
                                            }
                                        }
                                        other_attribute_value => {
                                            child_variable.set_value(VariableValue::Error(
                                                format!(
                                            "Unimplemented: Attribute Value for DW_AT_type {:?}",
                                            other_attribute_value
                                        ),
                                            ));
                                        }
                                    }
                                }
                                None => {
                                    child_variable.set_value(VariableValue::Error(format!(
                                    "Error: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                )));
                                }
                            }
                        }
                        Err(error) => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Error: Failed to decode pointer reference: {:?}",
                                error
                            )));
                        }
                    }
                }
                gimli::DW_TAG_structure_type => {
                    if let Some(child_member_index) = child_variable.member_index {
                        match &parent_variable.memory_location {
                            variable::VariableLocation::Address(address) => {
                                // This is a member of an array type, and needs special handling.
                                let (location, has_overflowed) = address.overflowing_add(
                                    child_member_index as u32 * child_variable.byte_size as u32,
                                );

                                // TODO:

                                if has_overflowed {
                                    return Err(DebugError::Other(anyhow::anyhow!(
                                        "Overflow calculating variable address"
                                    )));
                                } else {
                                    child_variable.memory_location =
                                        variable::VariableLocation::Address(location);
                                }
                            }
                            _other => {
                                child_variable.memory_location =
                                    variable::VariableLocation::Unavailable;
                            }
                        }
                    }

                    child_variable.type_name =
                        VariableType::Struct(type_name.unwrap_or_else(|| "<unnamed>".to_string()));

                    if child_variable.memory_location != variable::VariableLocation::Unavailable {
                        if child_variable.variable_node_type == VariableNodeType::RecurseToBaseType
                        {
                            // In some cases, it really simplifies the UX if we can auto resolve the children and dreive a value that is visible at first glance to the user.
                            child_variable = self.process_tree(
                                node,
                                child_variable,
                                core,
                                stack_frame_registers,
                                cache,
                            )?;
                        } else {
                            // Defer the processing of child types.
                            child_variable.variable_node_type =
                                VariableNodeType::TypeOffset(node.entry().offset());
                        }
                    } else {
                        // If something is already broken, then do nothing ...
                        child_variable.variable_node_type = VariableNodeType::DoNotRecurse;
                    }
                }
                gimli::DW_TAG_enumeration_type => {
                    child_variable.type_name =
                        VariableType::Enum(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                    // Recursively process a child types.
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        core,
                        stack_frame_registers,
                        cache,
                    )?;
                    if parent_variable.is_valid() && child_variable.is_valid() {
                        let enumerator_values =
                            cache.get_children(Some(child_variable.variable_key))?;

                        if let variable::VariableLocation::Address(address) =
                            child_variable.memory_location
                        {
                            // NOTE: hard-coding value of variable.byte_size to 1 ... replace with code if necessary.
                            let mut buff = [0u8; 1];
                            core.read(address, &mut buff)?;
                            let this_enum_const_value = u8::from_le_bytes(buff).to_string();
                            let enumumerator_value =
                                match enumerator_values.into_iter().find(|enumerator_variable| {
                                    enumerator_variable.get_value(cache) == this_enum_const_value
                                }) {
                                    Some(this_enum) => this_enum.name,
                                    None => VariableName::Named(
                                        "<Error: Unresolved enum value>".to_string(),
                                    ),
                                };
                            child_variable.set_value(VariableValue::Valid(format!(
                                "{}::{}",
                                child_variable.type_name, enumumerator_value
                            )));
                            // We don't need to keep these children.
                            cache.remove_cache_entry_children(child_variable.variable_key)?;
                        } else {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unsupported variable location {:?}",
                                child_variable.memory_location
                            )));

                            // We don't need to keep these children.
                            cache.remove_cache_entry_children(child_variable.variable_key)?;
                        }
                    }
                }
                gimli::DW_TAG_array_type => {
                    // This node is a pointer to the type of data stored in the array, with a direct child that contains the range information.
                    match node.entry().attr(gimli::DW_AT_type) {
                        Ok(optional_data_type_attribute) => {
                            match optional_data_type_attribute {
                                Some(data_type_attribute) => {
                                    match data_type_attribute.value() {
                                        gimli::AttributeValue::UnitRef(unit_ref) => {
                                            // First get the DW_TAG_subrange child of this node. It has a DW_AT_type that points to DW_TAG_base_type:__ARRAY_SIZE_TYPE__.
                                            let mut subrange_variable = cache.cache_variable(
                                                Some(child_variable.variable_key),
                                                Variable::new(
                                                    self.unit
                                                        .header
                                                        .offset()
                                                        .as_debug_info_offset(),
                                                    Some(node.entry().offset()),
                                                ),
                                                core,
                                            )?;

                                            subrange_variable = self.process_tree(
                                                node,
                                                subrange_variable,
                                                core,
                                                stack_frame_registers,
                                                cache,
                                            )?;
                                            if child_variable.is_valid() {
                                                child_variable.range_lower_bound =
                                                    subrange_variable.range_lower_bound;
                                                child_variable.range_upper_bound =
                                                    subrange_variable.range_upper_bound;
                                                if child_variable.range_lower_bound < 0
                                                    || child_variable.range_upper_bound < 0
                                                {
                                                    child_variable.set_value(VariableValue::Error(format!(
                                                    "Unimplemented: Array has a sub-range of {}..{} for ",
                                                    child_variable.range_lower_bound, child_variable.range_upper_bound)
                                                ));
                                                }
                                                cache.remove_cache_entry(
                                                    subrange_variable.variable_key,
                                                )?;
                                                // - Next, process this DW_TAG_array_type's DW_AT_type full tree.
                                                // - We have to do this repeatedly, for every array member in the range.
                                                for array_member_index in child_variable
                                                    .range_lower_bound
                                                    ..child_variable.range_upper_bound
                                                {
                                                    let mut array_member_type_tree =
                                                        self.unit.header.entries_tree(
                                                            &self.unit.abbreviations,
                                                            Some(unit_ref),
                                                        )?;

                                                    if let Ok(mut array_member_type_node) =
                                                        array_member_type_tree.root()
                                                    {
                                                        let mut array_member_variable = cache
                                                            .cache_variable(
                                                                Some(child_variable.variable_key),
                                                                Variable::new(
                                                                    self.unit
                                                                        .header
                                                                        .offset()
                                                                        .as_debug_info_offset(),
                                                                    Some(
                                                                        array_member_type_node
                                                                            .entry()
                                                                            .offset(),
                                                                    ),
                                                                ),
                                                                core,
                                                            )?;
                                                        array_member_variable = self
                                                            .process_tree_node_attributes(
                                                                &mut array_member_type_node,
                                                                &mut child_variable,
                                                                array_member_variable,
                                                                core,
                                                                stack_frame_registers,
                                                                cache,
                                                            )?;
                                                        child_variable.type_name =
                                                            VariableType::Array {
                                                                count: subrange_variable
                                                                    .range_upper_bound
                                                                    as usize,
                                                                entry_type: array_member_variable
                                                                    .name,
                                                            };
                                                        array_member_variable.member_index =
                                                            Some(array_member_index);
                                                        array_member_variable.name =
                                                            VariableName::Named(format!(
                                                                "__{}",
                                                                array_member_index
                                                            ));
                                                        array_member_variable.source_location =
                                                            child_variable.source_location.clone();
                                                        self.extract_type(
                                                            array_member_type_node,
                                                            &child_variable,
                                                            array_member_variable,
                                                            core,
                                                            stack_frame_registers,
                                                            cache,
                                                        )?;
                                                    }
                                                }
                                            }
                                        }
                                        other_attribute_value => {
                                            child_variable.set_value(VariableValue::Error(
                                                format!(
                                            "Unimplemented: Attribute Value for DW_AT_type {:?}",
                                            other_attribute_value
                                        ),
                                            ));
                                        }
                                    }
                                }
                                None => {
                                    child_variable.set_value(VariableValue::Error(format!(
                                    "Error: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                )));
                                }
                            }
                        }
                        Err(error) => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Error: Failed to decode pointer reference: {:?}",
                                error
                            )));
                        }
                    }
                }
                gimli::DW_TAG_union_type => {
                    if let Some(child_member_index) = child_variable.member_index {
                        match &parent_variable.memory_location {
                            variable::VariableLocation::Address(address) => {
                                // This is a member of an array type, and needs special handling.
                                let (location, has_overflowed) = address.overflowing_add(
                                    child_member_index as u32 * child_variable.byte_size as u32,
                                );

                                // TODO:

                                if has_overflowed {
                                    return Err(DebugError::Other(anyhow::anyhow!(
                                        "Overflow calculating variable address"
                                    )));
                                } else {
                                    child_variable.memory_location =
                                        variable::VariableLocation::Address(location);
                                }
                            }
                            _other => {
                                child_variable.memory_location =
                                    variable::VariableLocation::Unavailable;
                            }
                        }
                    }

                    // Recursively process a child types.
                    // TODO: The DWARF does not currently hold information that allows decoding of which UNION arm is instantiated, so we have to display all available.
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        core,
                        stack_frame_registers,
                        cache,
                    )?;
                    if child_variable.is_valid() && !cache.has_children(&child_variable)? {
                        // Empty structs don't have values.
                        child_variable.set_value(VariableValue::Valid(format!(
                            "{:?}",
                            child_variable.type_name.clone()
                        )));
                    }
                }
                gimli::DW_TAG_subroutine_type => {
                    // The type_name will be found in the DW_AT_TYPE child of this entry.
                    match node.entry().attr(gimli::DW_AT_type) {
                        Ok(optional_data_type_attribute) => match optional_data_type_attribute {
                            Some(data_type_attribute) => match data_type_attribute.value() {
                                gimli::AttributeValue::UnitRef(unit_ref) => {
                                    let subroutine_type_node = self
                                        .unit
                                        .header
                                        .entry(&self.unit.abbreviations, unit_ref)?;
                                    child_variable.type_name = match subroutine_type_node
                                        .attr(gimli::DW_AT_name)
                                    {
                                        Ok(optional_name_attr) => match optional_name_attr {
                                            Some(name_attr) => VariableType::Other(extract_name(
                                                self.debug_info,
                                                name_attr.value(),
                                            )),
                                            None => VariableType::Unknown,
                                        },
                                        Err(error) => VariableType::Other(format!(
                                            "Error: evaluating subroutine type name: {:?} ",
                                            error
                                        )),
                                    };
                                }
                                other_attribute_value => {
                                    child_variable.set_value(VariableValue::Error(format!(
                                        "Unimplemented: Attribute Value for DW_AT_type {:?}",
                                        other_attribute_value
                                    )));
                                }
                            },
                            None => {
                                // TODO: Better indication for no return value
                                child_variable.set_value(VariableValue::Valid(
                                    "<No Return Value>".to_string(),
                                ));
                                child_variable.type_name = VariableType::Unknown;
                            }
                        },
                        Err(error) => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Error: Failed to decode subroutine type reference: {:?}",
                                error
                            )));
                        }
                    }
                }
                gimli::DW_TAG_compile_unit => {
                    // This only happens when we do a 'lazy' load of [VariableName::StaticScope]
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        core,
                        stack_frame_registers,
                        cache,
                    )?;
                }
                // Do not expand this type.
                other => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "<unimplemented: type : {:?}>",
                        other.static_string()
                    )));
                    child_variable.type_name = VariableType::Other("unimplemented".to_string());
                    cache.remove_cache_entry_children(child_variable.variable_key)?;
                }
            }
        }
        cache
            .cache_variable(Some(parent_variable.variable_key), child_variable, core)
            .map_err(|error| error.into())
    }

    /// - Consumes the `child_variable`.
    /// - Find the location using either DW_AT_location, or DW_AT_data_member_location, and store it in the Variable.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    ///
    /// This will either set the memory location, or directly update the value of the variable, depending on the DWARF information.
    fn extract_location(
        &self,
        node: &gimli::EntriesTreeNode<debug_info::GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &registers::Registers,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        let mut attrs = node.entry().attrs();
        while let Ok(Some(attr)) = attrs.next() {
            match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_data_member_location
                | gimli::DW_AT_frame_base => match attr.value() {
                    gimli::AttributeValue::Exprloc(expression) => {
                        if let Err(error) = self.evaluate_expression(
                            core,
                            &mut child_variable,
                            expression,
                            stack_frame_registers,
                        ) {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Error: Determining memory location for this variable: {:?}",
                                &error
                            )));
                        }
                    }
                    gimli::AttributeValue::Udata(offset_from_parent) => {
                        match &parent_variable.memory_location {
                            variable::VariableLocation::Address(address) => {
                                child_variable.memory_location = variable::VariableLocation::Address(
                                    address + offset_from_parent as u32,
                                )
                            }
                            _other => {
                                child_variable.memory_location =
                                    variable::VariableLocation::Unavailable;
                            }
                        }
                    }
                    gimli::AttributeValue::LocationListsRef(location_list_offset) => {
                        match self.debug_info.locations_section.locations(
                            location_list_offset,
                            self.unit.header.encoding(),
                            self.unit.low_pc,
                            &self.debug_info.address_section,
                            self.unit.addr_base,
                        ) {
                            Ok(mut locations) => {
                                let program_counter =
                                    stack_frame_registers.get_program_counter().unwrap_or(0) as u64;
                                let mut expression: Option<
                                    gimli::Expression<debug_info::GimliReader>,
                                > = None;
                                while let Some(location) = match locations.next() {
                                    Ok(location_lists_entry) => location_lists_entry,
                                    Err(error) => {
                                        child_variable.set_value(VariableValue::Error(format!("Error: Iterating LocationLists for this variable: {:?}", &error)));
                                        None
                                    }
                                } {
                                    if program_counter >= location.range.begin
                                        && program_counter < location.range.end
                                    {
                                        expression = Some(location.data);
                                        break;
                                    }
                                }

                                if let Some(valid_expression) = expression {
                                    if let Err(error) = self.evaluate_expression(
                                        core,
                                        &mut child_variable,
                                        valid_expression,
                                        stack_frame_registers,
                                    ) {
                                        child_variable.set_value(VariableValue::Error(format!("Error: Determining memory location for this variable: {:?}", &error)));
                                    }
                                } else {
                                    child_variable.set_value(VariableValue::Error(
                                        "<value out of scope - moved or dropped>".to_string(),
                                    ));
                                }
                            }
                            Err(error) => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Error: Resolving variable Location: {:?}",
                                    &error
                                )));
                            }
                        };
                    }
                    other_attribute_value => {
                        child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: extract_location() Could not extract location from: {:?}",
                                other_attribute_value
                            )));
                    }
                },
                gimli::DW_AT_address_class => {
                    match attr.value() {
                        gimli::AttributeValue::AddressClass(address_class) => {
                            // Nothing to do in this case where it is zero
                            if address_class != gimli::DwAddr(0) {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({:?}))",
                                    address_class
                                )));
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: extract_location() found invalid DW_AT_address_class: {:?}",
                                other_attribute_value
                            )));
                        }
                    }
                }
                _other_attributes => {
                    // These will be handled elsewhere.
                }
            }
        }

        cache
            .cache_variable(child_variable.parent_key, child_variable, core)
            .map_err(|error| error.into())
    }

    /// Evaluate a gimli::Expression as a valid memory location
    fn evaluate_expression(
        &self,
        core: &mut Core<'_>,
        child_variable: &mut Variable,
        expression: gimli::Expression<debug_info::GimliReader>,
        stack_frame_registers: &registers::Registers,
    ) -> Result<(), DebugError> {
        let pieces = self.expression_to_piece(core, expression, stack_frame_registers)?;
        if pieces.is_empty() {
            return Err(DebugError::Other(anyhow::anyhow!(
                "Error: expr_to_piece() returned 0 results: {:?}",
                pieces
            )));
        } else if pieces.len() > 1 {
            child_variable.set_value(VariableValue::Error(
                "<unsupported memory implementation>".to_string(),
            ));
            child_variable.memory_location = variable::VariableLocation::Unsupported(
                "<unsupported memory implementation>".to_string(),
            );
        } else {
            match &pieces[0].location {
                Location::Empty => {
                    // This means the value was optimized away.
                    child_variable.set_value(VariableValue::Error(
                        "<value optimized away by compiler>".to_string(),
                    ));
                    child_variable.memory_location = variable::VariableLocation::Unavailable;
                }
                Location::Address { address } => {
                    if *address == u32::MAX as u64 {
                        return Err(DebugError::Other(anyhow::anyhow!("BUG: Cannot resolve due to rust-lang issue https://github.com/rust-lang/rust/issues/32574".to_string())));
                    } else {
                        child_variable.memory_location =
                            variable::VariableLocation::Address(*address as u32);
                    }
                }
                Location::Value { value } => {
                    match value {
                        gimli::Value::Generic(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::I8(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::U8(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::I16(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::U16(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::I32(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::U32(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::I64(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::U64(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::F32(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                        gimli::Value::F64(value) => {
                            child_variable.set_value(VariableValue::Valid(value.to_string()));
                        }
                    };
                    child_variable.memory_location = variable::VariableLocation::Value;
                }
                Location::Register { register } => {
                    child_variable.memory_location =
                        variable::VariableLocation::Register(register.0 as usize);
                }
                l => {
                    return Err(DebugError::Other(anyhow::anyhow!(
                        "Unimplemented: extract_location() found a location type: {:?}",
                        l
                    )));
                }
            }
        }
        Ok(())
    }

    /// Update a [Variable] location, given a gimli::Expression
    fn expression_to_piece(
        &self,
        core: &mut Core<'_>,
        expression: gimli::Expression<debug_info::GimliReader>,
        stack_frame_registers: &registers::Registers,
    ) -> Result<Vec<gimli::Piece<debug_info::GimliReader, usize>>, DebugError> {
        let mut evaluation = expression.evaluation(self.unit.encoding());
        let frame_base = if let Some(frame_base) = stack_frame_registers.get_frame_pointer() {
            u64::from(frame_base)
        } else {
            return Err(DebugError::Other(anyhow::anyhow!(
                "Cannot unwind `Variable` location without a valid CFA (canonical frame address)"
            )));
        };
        // go for evaluation
        let mut result = evaluation.evaluate()?;

        loop {
            use gimli::EvaluationResult::*;

            result = match result {
                Complete => break,
                RequiresMemory { address, size, .. } => {
                    let mut buff = vec![0u8; size as usize];
                    core.read(address as u32, &mut buff).map_err(|_| {
                        DebugError::Other(anyhow::anyhow!("Unexpected error while reading debug expressions from target memory. Please report this as a bug."))
                    })?;
                    match size {
                        1 => evaluation.resume_with_memory(gimli::Value::U8(buff[0]))?,
                        2 => {
                            let val = (u16::from(buff[0]) << 8) | (u16::from(buff[1]) as u16);
                            evaluation.resume_with_memory(gimli::Value::U16(val))?
                        }
                        4 => {
                            let val = (u32::from(buff[0]) << 24)
                                | (u32::from(buff[1]) << 16)
                                | (u32::from(buff[2]) << 8)
                                | u32::from(buff[3]);
                            evaluation.resume_with_memory(gimli::Value::U32(val))?
                        }
                        x => {
                            todo!(
                                "Requested memory with size {}, which is not supported yet.",
                                x
                            );
                        }
                    }
                }
                RequiresFrameBase => match evaluation.resume_with_frame_base(frame_base) {
                    Ok(evaluation_result) => evaluation_result,
                    Err(error) => {
                        return Err(DebugError::Other(anyhow::anyhow!(
                            "Error while calculating `Variable::memory_location`:{}.",
                            error
                        )))
                    }
                },
                RequiresRegister {
                    register,
                    base_type,
                } => {
                    let raw_value = match stack_frame_registers
                        .get_value_by_dwarf_register_number(register.0 as u32)
                    {
                        Some(raw_value) => {
                            if base_type != gimli::UnitOffset(0) {
                                return Err(DebugError::Other(anyhow::anyhow!(
                                    "Unimplemented: Support for type {:?} in `RequiresRegister` request is not yet implemented.",
                                    base_type
                                )));
                            }
                            raw_value
                        }
                        None => {
                            return Err(DebugError::Other(anyhow::anyhow!(
                                    "Error while calculating `Variable::memory_location`. No value for register #:{}.",
                                    register.0
                                )));
                        }
                    };

                    evaluation.resume_with_register(gimli::Value::Generic(raw_value as u64))?
                }
                RequiresRelocatedAddress(address_index) => {
                    if address_index.is_zero() {
                        // This is a rust-lang bug for statics ... https://github.com/rust-lang/rust/issues/32574.
                        evaluation.resume_with_relocated_address(u64::MAX)?
                    } else {
                        // The address_index as an offset from 0, so just pass it into the next step.
                        evaluation.resume_with_relocated_address(address_index)?
                    }
                }
                unimplemented_expression => {
                    return Err(DebugError::Other(anyhow::anyhow!(
                        "Unimplemented: Expressions that include {:?} are not currently supported.",
                        unimplemented_expression
                    )));
                }
            }
        }
        Ok(evaluation.result())
    }
}

/// If file information is available, it returns `Some(directory:PathBuf, file_name:String)`, otherwise `None`.
fn extract_file(
    debug_info: &debug_info::DebugInfo,
    unit: &gimli::Unit<debug_info::GimliReader>,
    attribute_value: gimli::AttributeValue<debug_info::GimliReader>,
) -> Option<(PathBuf, String)> {
    match attribute_value {
        gimli::AttributeValue::FileIndex(index) => unit.line_program.as_ref().and_then(|ilnp| {
            let header = ilnp.header();

            if let Some(file_entry) = header.file(index) {
                if let Some((Some(path), Some(file))) = debug_info
                    .find_file_and_directory(unit, header, file_entry)
                    .map(|(file, path)| (path, file))
                {
                    Some((path, file))
                } else {
                    log::warn!("Unable to extract file or path from {:?}.", attribute_value);
                    None
                }
            } else {
                log::warn!("Unable to extract file entry for {:?}.", attribute_value);
                None
            }
        }),
        other => {
            log::warn!(
                "Unable to extract file information from attribute value {:?}: Not implemented.",
                other
            );
            None
        }
    }
}

/// If a DW_AT_byte_size attribute exists, return the u64 value, otherwise (including errors) return 0
fn extract_byte_size(
    _debug_info: &debug_info::DebugInfo,
    di_entry: &DebuggingInformationEntry<debug_info::GimliReader>,
) -> u64 {
    match di_entry.attr(gimli::DW_AT_byte_size) {
        Ok(optional_byte_size_attr) => match optional_byte_size_attr {
            Some(byte_size_attr) => match byte_size_attr.value() {
                gimli::AttributeValue::Udata(byte_size) => byte_size,
                other => {
                    log::warn!("Unimplemented: DW_AT_byte_size value: {:?} ", other);
                    0
                }
            },
            None => 0,
        },
        Err(error) => {
            log::warn!(
                "Failed to extract byte_size: {:?} for debug_entry {:?}",
                error,
                di_entry.tag().static_string()
            );
            0
        }
    }
}

fn extract_line(attribute_value: gimli::AttributeValue<debug_info::GimliReader>) -> Option<u64> {
    match attribute_value {
        gimli::AttributeValue::Udata(line) => Some(line),
        _ => None,
    }
}

fn extract_name(
    debug_info: &debug_info::DebugInfo,
    attribute_value: gimli::AttributeValue<debug_info::GimliReader>,
) -> String {
    match attribute_value {
        gimli::AttributeValue::DebugStrRef(name_ref) => {
            if let Ok(name_raw) = debug_info.dwarf.string(name_ref) {
                String::from_utf8_lossy(&name_raw).to_string()
            } else {
                "Invalid DW_AT_name value".to_string()
            }
        }
        gimli::AttributeValue::String(name) => String::from_utf8_lossy(&name).to_string(),
        other => format!("Unimplemented: Evaluate name from {:?}", other),
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) fn _print_all_attributes(
    core: &mut Core<'_>,
    stackframe_cfa: Option<u64>,
    dwarf: &gimli::Dwarf<debug_info::DwarfReader>,
    unit: &gimli::Unit<debug_info::DwarfReader>,
    tag: &gimli::DebuggingInformationEntry<debug_info::DwarfReader>,
    print_depth: usize,
) {
    let mut attrs = tag.attrs();

    while let Some(attr) = attrs.next().unwrap() {
        for _ in 0..(print_depth) {
            print!("\t");
        }
        print!("{}: ", attr.name());

        use gimli::AttributeValue::*;

        match attr.value() {
            Addr(a) => println!("{:#010x}", a),
            DebugStrRef(_) => {
                let val = dwarf.attr_string(unit, attr.value()).unwrap();
                println!("{}", std::str::from_utf8(&val).unwrap());
            }
            Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                loop {
                    use gimli::EvaluationResult::*;

                    result = match result {
                        Complete => break,
                        RequiresMemory { address, size, .. } => {
                            let mut buff = vec![0u8; size as usize];
                            core.read(address as u32, &mut buff)
                                .expect("Failed to read memory");
                            match size {
                                1 => evaluation
                                    .resume_with_memory(gimli::Value::U8(buff[0]))
                                    .unwrap(),
                                2 => {
                                    let val = u16::from(buff[0]) << 8 | u16::from(buff[1]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U16(val))
                                        .unwrap()
                                }
                                4 => {
                                    let val = u32::from(buff[0]) << 24
                                        | u32::from(buff[1]) << 16
                                        | u32::from(buff[2]) << 8
                                        | u32::from(buff[3]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U32(val))
                                        .unwrap()
                                }
                                x => {
                                    log::error!(
                                        "Requested memory with size {}, which is not supported yet.",
                                        x
                                    );
                                    unimplemented!();
                                }
                            }
                        }
                        RequiresFrameBase => evaluation
                            .resume_with_frame_base(stackframe_cfa.unwrap())
                            .unwrap(),
                        RequiresRegister {
                            register,
                            base_type,
                        } => {
                            let raw_value = core
                                .read_core_reg(register.0 as u16)
                                .expect("Failed to read memory");

                            if base_type != gimli::UnitOffset(0) {
                                unimplemented!(
                                    "Support for units in RequiresRegister request is not yet implemented."
                                )
                            }
                            evaluation
                                .resume_with_register(gimli::Value::Generic(raw_value as u64))
                                .unwrap()
                        }
                        RequiresRelocatedAddress(address_index) => {
                            if address_index.is_zero() {
                                // This is a rust-lang bug for statics ... https://github.com/rust-lang/rust/issues/32574;
                                evaluation.resume_with_relocated_address(u64::MAX).unwrap()
                            } else {
                                // Use the address_index as an offset from 0, so just pass it into the next step.
                                evaluation
                                    .resume_with_relocated_address(address_index)
                                    .unwrap()
                            }
                        }
                        x => {
                            println!("print_all_attributes {:?}", x);
                            // x
                            todo!()
                        }
                    }
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            }
            LocationListsRef(_) => {
                println!("LocationList");
            }
            DebugLocListsBase(_) => {
                println!(" LocationList");
            }
            DebugLocListsIndex(_) => {
                println!(" LocationList");
            }
            _ => {
                println!("print_all_attributes {:?}", attr.value());
            }
        }
    }
}
