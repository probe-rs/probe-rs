use crate::debug::{language::ProgrammingLanguage, unit_info::UnitInfo};

use super::*;
use gimli::{DebugInfoOffset, DwLang, UnitOffset};
use itertools::Itertools;
use std::ops::Range;

/// Define the role that a variable plays in a Variant relationship. See section '5.7.10 Variant
/// Entries' of the DWARF 5 specification
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub enum VariantRole {
    /// A (parent) Variable that can have any number of Variant's as its value
    VariantPart(u64),
    /// A (child) Variable that defines one of many possible types to hold the current value of a
    /// VariantPart.
    Variant(u64),
    /// This variable doesn't play a role in a Variant relationship
    #[default]
    NonVariant,
}

/// A [Variable] will have either a valid value, or some reason why a value could not be constructed.
/// - If we encounter expected errors, they will be displayed to the user as defined below.
/// - If we encounter unexpected errors, they will be treated as proper errors and will propagated
///   to the calling process as an `Err()`
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum VariableValue {
    /// A valid value of this variable
    Valid(String),
    /// Notify the user that we encountered a problem correctly resolving the variable.
    /// - The variable will be visible to the user, as will the other field of the variable.
    /// - The contained warning message will be displayed to the user.
    /// - The debugger will not attempt to resolve additional fields or children of this variable.
    Error(String),
    /// The value has not been set. This could be because ...
    /// - It is too early in the process to have discovered its value, or ...
    /// - The variable cannot have a stored value, e.g. a `struct`. In this case, please use
    ///   `Variable::get_value` to infer a human readable value from the value of the struct's fields.
    #[default]
    Empty,
}

impl std::fmt::Display for VariableValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableValue::Valid(value) => value.fmt(f),
            VariableValue::Error(error) => write!(f, "< {error} >"),
            VariableValue::Empty => write!(
                f,
                "Value not set. Please use Variable::get_value() to infer a human readable variable value"
            ),
        }
    }
}

impl VariableValue {
    /// Returns `true` if the variable resolver did not encounter an error, `false` otherwise.
    pub fn is_valid(&self) -> bool {
        !matches!(self, VariableValue::Error(_))
    }

    /// Returns `true` if no value or error is present, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        matches!(self, VariableValue::Empty)
    }
}

/// The type of variable we have at hand.
#[derive(Debug, PartialEq, Eq, Clone, Default, Serialize)]
pub enum VariableName {
    /// Top-level variable for static variables, child of a stack frame variable,
    /// and holds all the static scoped variables which are directly visible to the
    /// compile unit of the frame.
    StaticScopeRoot,
    /// Top-level variable for registers, child of a stack frame variable.
    RegistersRoot,
    /// Top-level variable for local scoped variables, child of a stack frame variable.
    LocalScopeRoot,
    /// Artificial variable, without a name (e.g. enum discriminant)
    Artifical,
    /// Anonymous namespace
    AnonymousNamespace,
    /// A Namespace with a specific name
    Namespace(String),
    /// Variable with a specific name
    Named(String),
    /// Variable with an unknown name
    #[default]
    Unknown,
}

impl std::fmt::Display for VariableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableName::StaticScopeRoot => write!(f, "Static Variable"),
            VariableName::RegistersRoot => write!(f, "Platform Register"),
            VariableName::LocalScopeRoot => write!(f, "Function Variable"),
            VariableName::Artifical => write!(f, "<artifical>"),
            VariableName::AnonymousNamespace => write!(f, "<anonymous_namespace>"),
            VariableName::Namespace(name) => name.fmt(f),
            VariableName::Named(name) => name.fmt(f),
            VariableName::Unknown => write!(f, "<unknown>"),
        }
    }
}

/// Encode the nature of the Debug Information Entry in a way that we can resolve child nodes of a
/// [Variable].
///
/// The rules for 'lazy loading'/deferred recursion of [Variable] children are described under each
/// of the enum values.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub enum VariableNodeType {
    /// Use the `header_offset` and `type_offset` as direct references for recursing the variable
    /// children. With the current implementation, the `type_offset` will point to a DIE with a tag
    /// of `DW_TAG_structure_type`.
    /// - Rule: For structured variables, we WILL NOT automatically expand their children, but we
    ///         have enough information to expand it on demand. Except if they fall into one of the
    ///         special cases handled by [VariableNodeType::RecurseToBaseType]
    TypeOffset(DebugInfoOffset, UnitOffset),
    /// Use the `header_offset` and `entries_offset` as direct references for recursing the variable
    /// children.
    /// - Rule: All top level variables in a [StackFrame] are automatically deferred, i.e
    /// [VariableName::LocalScopeRoot], [VariableName::RegistersRoot].
    DirectLookup(DebugInfoOffset, UnitOffset),
    /// Look up information from all compilation units. This is used to resolve static variables, so
    /// when [`VariableName::StaticScopeRoot`] is used.
    UnitsLookup,
    /// Sometimes it doesn't make sense to recurse the children of a specific node type
    /// - Rule: Pointers to `unit` datatypes WILL NOT BE resolved, because it doesn't make sense.
    /// - Rule: Once we determine that a variable can not be recursed further, we update the
    ///         variable_node_type to indicate that no further recursion is possible/required. This
    ///         can be because the variable is a 'base' data type, or because there was some kind of
    ///         error in processing the current node, so we don't want to incur cascading errors.
    /// TODO: Find code instances where we use magic values (e.g. u32::MAX) and replace with DoNotRecurse logic if appropriate.
    DoNotRecurse,
    /// Unless otherwise specified, always recurse the children of every node until we get to the
    /// base data type.
    /// - Rule: (Default) Unless it is prevented by any of the other rules, we always recurse the
    ///         children of these variables.
    /// - Rule: Certain structured variables (e.g. `&str`, `Some`, `Ok`, `Err`, etc.) are set to
    ///         [VariableNodeType::RecurseToBaseType] to improve the debugger UX.
    /// - Rule: Pointers to `const` variables WILL ALWAYS BE recursed, because they provide
    ///         essential information, for example about the length of strings, or the size of
    ///         arrays.
    /// - Rule: Enumerated types WILL ALWAYS BE recursed, because we only ever want to see the
    ///         'active' child as the value.
    /// - Rule: For now, Array types WILL ALWAYS BE recursed. TODO: Evaluate if it is beneficial to
    ///         defer these.
    /// - Rule: For now, Union types WILL ALWAYS BE recursed. TODO: Evaluate if it is beneficial to
    ///         defer these.
    #[default]
    RecurseToBaseType,
}

impl VariableNodeType {
    /// Will return `true` if the `variable_node_type` value implies that the variable will be
    /// 'lazy' resolved.
    pub fn is_deferred(&self) -> bool {
        match self {
            VariableNodeType::TypeOffset(_, _)
            | VariableNodeType::DirectLookup(_, _)
            | VariableNodeType::UnitsLookup => true,
            VariableNodeType::DoNotRecurse | VariableNodeType::RecurseToBaseType => false,
        }
    }
}

/// The starting bit (and direction) of a bit field type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BitOffset {
    /// The bit offset is from the least significant bit.
    FromLsb(u64),
    /// The bit offset is from the most significant bit.
    FromMsb(u64),
}

/// Bitfield information for a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bitfield {
    /// The starting bit (and direction) of a bit field type.
    pub offset: BitOffset,
    /// The length of the bit field.
    pub length: u64,
}

impl Default for Bitfield {
    fn default() -> Self {
        Bitfield {
            offset: BitOffset::FromLsb(0),
            length: 0,
        }
    }
}

impl Bitfield {
    /// Returns a Bitfield that has a FromLsb offset.
    pub(crate) fn normalize(&self, byte_size: u64) -> Self {
        let offset = self.offset(byte_size);
        Bitfield {
            offset: BitOffset::FromLsb(offset),
            length: self.length,
        }
    }

    pub(crate) fn offset(&self, byte_size: u64) -> u64 {
        match self.offset {
            BitOffset::FromLsb(offset) => offset,
            BitOffset::FromMsb(offset) => byte_size * 8 - offset - self.length,
        }
    }

    pub(crate) fn normalized_offset(&self) -> u64 {
        match self.offset {
            BitOffset::FromLsb(offset) => offset,
            BitOffset::FromMsb(_) => unreachable!("Bitfield should have been normalized first"),
        }
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn mask(&self) -> u128 {
        (1 << self.length) - 1
    }

    pub(crate) fn extract(&self, value: u128) -> u128 {
        let offset = self.normalized_offset();
        let mask = self.mask();

        (value >> offset) & mask
    }

    pub(crate) fn insert(&self, value: u128, new_value: u128) -> u128 {
        let offset = self.normalized_offset();
        let mask = self.mask();

        let shifted_mask = mask << offset;
        let new_value = (new_value & mask) << offset;
        (value & !shifted_mask) | new_value
    }
}

/// A modifier to a variable type. Currently only used to format the type name.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub enum Modifier {
    /// The type is declared as `volatile`.
    Volatile,

    /// The type is declared as `const`.
    Const,

    /// The type is declared as `restrict`.
    Restrict,

    /// The type is declared as `atomic`.
    Atomic,

    /// The type is an alias with the given name.
    Typedef(String),
}

/// The variants of VariableType allows us to streamline the conditional logic that requires
/// specific handling depending on the nature of the variable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub enum VariableType {
    /// A variable with a Rust base datatype.
    Base(String),
    /// The variable is a range of bits in a wider (integer) type.
    Bitfield(Bitfield, Box<VariableType>),
    /// A Rust struct.
    Struct(String),
    /// A Rust enum.
    Enum(String),
    /// Namespace refers to the path that qualifies a variable. e.g. "std::string" is the namespace
    /// for the struct "String"
    Namespace,
    /// A Pointer is a variable that contains a reference to another variable, and the type of the
    /// referenced variable may not be known until the reference has been resolved.
    Pointer(Option<String>),
    /// A Rust array.
    Array {
        /// The type name of the variable.
        item_type_name: Box<VariableType>,
        /// The number of entries in the array.
        count: usize,
    },
    /// A type alias.
    Modified(Modifier, Box<VariableType>),
    /// When we are unable to determine the name of a variable.
    #[default]
    Unknown,
    /// For infrequently used categories of variables that does not fall into any of the other
    /// `VariableType` variants.
    Other(String),
}

impl VariableType {
    /// Get the inner type of a modified type.
    pub fn inner(&self) -> &Self {
        if let Self::Modified(_, ty) = self {
            ty.inner()
        } else {
            self
        }
    }

    /// Get the inner type of a modified type, stopping at typedef aliases.
    fn skip_modifiers(&self) -> &Self {
        match self {
            Self::Modified(Modifier::Typedef(_), _) => self,
            Self::Modified(_, ty) => ty.skip_modifiers(),
            _ => self,
        }
    }

    /// Is this variable of a Rust PhantomData marker type?
    pub fn is_phantom_data(&self) -> bool {
        match self {
            VariableType::Struct(name) => name.starts_with("PhantomData"),
            _ => false,
        }
    }

    /// Is this variable an array?
    pub fn is_array(&self) -> bool {
        matches!(self, VariableType::Array { .. })
    }

    /// Returns the string representation of the variable type's kind.
    pub fn kind(&self) -> &str {
        match self {
            VariableType::Base(_) => "base",
            VariableType::Bitfield(..) => "bitfield",
            VariableType::Struct(_) => "struct",
            VariableType::Enum(_) => "enum",
            VariableType::Namespace => "namespace",
            VariableType::Pointer(_) => "pointer",
            VariableType::Array { .. } => "array",
            VariableType::Unknown => "unknown",
            VariableType::Other(_) => "other",
            VariableType::Modified(_, inner) => inner.kind(),
        }
    }

    pub(crate) fn display_name(&self, language: &dyn ProgrammingLanguage) -> String {
        match self {
            VariableType::Modified(Modifier::Typedef(name), _) => name.clone(),
            VariableType::Modified(modifier, ty) => {
                language.modified_type_name(modifier, &ty.display_name(language))
            }

            VariableType::Array {
                item_type_name,
                count,
            } => language.format_array_type(
                // In case the compiler points at a modified item type (e.g. const), skip the
                // modifier.
                &item_type_name.skip_modifiers().display_name(language),
                *count,
            ),

            VariableType::Bitfield(bitfield, name) => {
                language.format_bitfield_type(&name.display_name(language), *bitfield)
            }

            _ => self.type_name(language),
        }
    }

    /// Returns the type name after resolving aliases.
    pub(crate) fn type_name(&self, language: &dyn ProgrammingLanguage) -> String {
        let type_name = match self {
            VariableType::Base(name)
            | VariableType::Struct(name)
            | VariableType::Enum(name)
            | VariableType::Other(name) => Some(name.as_str()),

            VariableType::Namespace => Some("namespace"),
            VariableType::Unknown => None,

            VariableType::Pointer(pointee) => {
                // TODO: we should also carry the constness
                return language.format_pointer_type(pointee.as_deref());
            }

            VariableType::Array {
                item_type_name,
                count,
            } => return language.format_array_type(&item_type_name.type_name(language), *count),

            VariableType::Bitfield(_, ty) | VariableType::Modified(_, ty) => {
                return ty.type_name(language)
            }
        };

        type_name.unwrap_or("<unknown>").to_string()
    }
}

/// Location of a variable
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VariableLocation {
    /// Location of the variable is not known. This means that it has not been evaluated yet.
    #[default]
    Unknown,
    /// The variable does not have a location currently, probably due to optimisations.
    Unavailable,
    /// The variable can be found in memory, at this address.
    Address(u64),
    /// The value of the variable is directly available.
    Value,
    /// There was an error evaluating the variable location.
    Error(String),
    /// Support for handling the location of this variable is not (yet) implemented.
    Unsupported(String),
}

impl VariableLocation {
    /// Return the memory address, if available. Otherwise an error is returned.
    pub fn memory_address(&self) -> Result<u64, DebugError> {
        match self {
            VariableLocation::Address(address) => Ok(*address),
            other => Err(DebugError::WarnAndContinue {
                message: format!("Variable does not have a memory location: location={other:?}"),
            }),
        }
    }

    /// Check if the location is valid, ie. not an error, unsupported, or unavailable.
    pub fn valid(&self) -> bool {
        match self {
            VariableLocation::Address(_) | VariableLocation::Value | VariableLocation::Unknown => {
                true
            }
            _other => false,
        }
    }
}

impl std::fmt::Display for VariableLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableLocation::Unknown => "<unknown value>".fmt(f),
            VariableLocation::Unavailable => "<value not available>".fmt(f),
            VariableLocation::Address(address) => write!(f, "{address:#010X}"),
            VariableLocation::Value => "<not applicable - statically stored value>".fmt(f),
            VariableLocation::Error(error) => error.fmt(f),
            VariableLocation::Unsupported(reason) => reason.fmt(f),
        }
    }
}

/// The `Variable` struct is used in conjunction with `VariableCache` to cache data about variables.
///
/// Any modifications to the `Variable` value will be transient (lost when it goes out of scope),
/// unless it is updated through one of the available methods on `VariableCache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Every variable must have a unique key value assigned to it.
    /// The value will be zero until it is stored in VariableCache, at which time its value will be
    /// set to the same as the VariableCache::variable_cache_key
    pub(super) variable_key: ObjectRef,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache.
    pub parent_key: ObjectRef,
    /// The variable name refers to the name of any of the types of values described in the [VariableCache]
    pub name: VariableName,
    /// Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    pub(super) value: VariableValue,
    /// The source location of the declaration of this variable, if available.
    pub source_location: SourceLocation,
    /// Programming language of the defining compilation unit.
    pub language: DwLang,

    /// The name of the type of this variable.
    pub type_name: VariableType,
    /// For 'lazy loading' of certain variable types we have to determine if the variable recursion
    /// should be deferred, and if so, how to resolve it when the request for further recursion
    /// happens.
    /// See [VariableNodeType] for more information.
    pub variable_node_type: VariableNodeType,
    /// The starting location/address in memory where this Variable's value is stored.
    pub memory_location: VariableLocation,
    /// The size of this variable in bytes.
    pub byte_size: Option<u64>,
    /// If this is a subrange (array, vector, etc.), is the ordinal position of this variable in
    /// that range
    pub member_index: Option<i64>,
    /// The role of this variable.
    pub role: VariantRole,
}

impl Variable {
    /// In most cases, Variables will be initialized with their ELF references so that we resolve
    /// their data types and values on demand.
    pub fn new(unit_info: Option<&UnitInfo>) -> Variable {
        Variable {
            language: unit_info
                .map(|info| info.get_language())
                .unwrap_or(gimli::DW_LANG_Rust),
            variable_key: Default::default(),
            parent_key: Default::default(),
            name: Default::default(),
            value: Default::default(),
            source_location: Default::default(),
            type_name: Default::default(),
            variable_node_type: Default::default(),
            memory_location: Default::default(),
            byte_size: None,
            member_index: None,
            role: Default::default(),
        }
    }

    /// Returns the readable name of the variable type.
    pub fn type_name(&self) -> String {
        self.type_name
            .display_name(language::from_dwarf(self.language).as_ref())
    }

    /// Get a unique key for this variable.
    pub fn variable_key(&self) -> ObjectRef {
        self.variable_key
    }

    /// This ensures debug frontends can see the errors, but doesn't fail because of a single
    /// variable not being able to decode correctly.
    pub fn set_value(&mut self, new_value: VariableValue) {
        // Allow some block when logic requires it.
        if new_value.is_valid() || self.value.is_valid() {
            // Simply overwrite existing value with a new valid one.
            self.value = new_value;
        } else {
            // Concatenate the error messages ...
            self.value = VariableValue::Error(format!("{} : {}", self.value, new_value));

            // If the value is invalid, then make sure we don't propagate invalid memory location
            // values.
            self.memory_location = VariableLocation::Unavailable;
        }
    }

    /// Convert the [String] value into the appropriate memory format and update the target memory
    /// with the new value.
    /// Currently this only works for base data types. There is no provision in the MS DAP API to
    /// catch this client side, so we can only respond with a 'gentle' error message if the user
    /// attempts unsupported data types.
    pub fn update_value(
        &self,
        memory: &mut impl MemoryInterface,
        variable_cache: &mut VariableCache,
        new_value: String,
    ) -> Result<(), DebugError> {
        let valid_value = self.is_valid();
        let valid_type = self.type_name != VariableType::Unknown;
        let valid_memory = self.memory_location.valid();
        if !valid_value || !valid_type || !valid_memory {
            // Insufficient data available.
            Err(DebugError::Other(format!(
                "Cannot update variable: {:?}, with supplied information (value={:?}, type={:?}, memory location={:#010x?}).",
                self.name, self.value, self.type_name, self.memory_location
            )))
        } else {
            // We have everything we need to update the variable value.
            language::from_dwarf(self.language)
                .update_variable(self, memory, &new_value)
                .map_err(|error| DebugError::WarnAndContinue {
                    message: format!("Invalid data value={new_value:?}: {error}"),
                })?;

            // Now update the cache with the new value for this variable.
            let mut cache_variable = self.clone();
            cache_variable.value = VariableValue::Valid(new_value);
            cache_variable.extract_value(memory, variable_cache);
            variable_cache.update_variable(&cache_variable)?;
            Ok(())
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of
    /// updating the value without overriding earlier values ... see set_value()).
    pub fn to_string(&self, variable_cache: &VariableCache) -> String {
        // Allow for chained `if let` without complaining
        if !self.value.is_empty() {
            // The `value` for this `Variable` is non empty because either
            // - It is base data type for which a value was determined based on the core runtime
            // - We encountered an error somewhere, so report it to the user
            format!("{}", self.value)
        } else if matches!(
            self.name,
            VariableName::AnonymousNamespace | VariableName::Namespace(_)
        ) {
            // Namespaces do not have values
            String::new()
        } else {
            // We need to construct a 'human readable' value using `fmt::Display` to represent the
            // values of complex types and pointers.
            if variable_cache.has_children(self) {
                self.formatted_variable_value(variable_cache, 0, false)
                    .unwrap_or_default()
            } else if self.type_name == VariableType::Unknown || !self.memory_location.valid() {
                if self.variable_node_type.is_deferred() {
                    // When we will do a lazy-load of variable children, and they have not yet been
                    // requested by the user, just display the type_name as the value
                    self.type_name
                        .display_name(language::from_dwarf(self.language).as_ref())
                } else {
                    // This condition should only be true for intermediate nodes
                    // from DWARF. These should not show up in the final
                    // `VariableCache`. If a user sees this error, then there is
                    // a logic problem in the stack unwind
                    "Error: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
                }
            } else if matches!(self.type_name, VariableType::Struct(ref name) if name == "None") {
                "None".to_string()
            } else if matches!(self.type_name, VariableType::Array { count: 0, .. }) {
                self.formatted_variable_value(variable_cache, 0, false)
                    .unwrap_or_default()
            } else {
                format!(
                    "Unimplemented: Get value of type {:?} of ({:?} bytes) at location {}",
                    self.type_name, self.byte_size, self.memory_location
                )
            }
        }
    }

    /// Evaluate the variable's result if possible and set self.value, or else set self.value as the error String.
    pub fn extract_value(
        &mut self,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) {
        if let VariableValue::Error(_) = self.value {
            // Nothing more to do ...
            return;
        }

        let empty = self.value.is_empty();
        // The value was set explicitly, so just leave it as is, or it was an error, so don't attempt
        // anything else
        let valid = self.memory_location.valid();
        // This may just be that we are early on in the process of `Variable` evaluation
        let unknown = self.type_name.inner() == &VariableType::Unknown;

        if !empty || !valid || unknown {
            return;
        }

        if self.variable_node_type.is_deferred()
            || matches!(self.type_name, VariableType::Pointer(_))
        {
            // And we have not previously assigned the value, then assign the type and address as
            // the value.
            self.value = VariableValue::Valid(format!(
                "{} @ {}",
                self.type_name
                    .display_name(language::from_dwarf(self.language).as_ref()),
                self.memory_location
            ));
            return;
        }

        tracing::trace!(
            "Extracting value for {:?}, type={:?}",
            self.name,
            self.type_name
        );

        self.value =
            language::from_dwarf(self.language).read_variable_value(self, memory, variable_cache);
    }

    /// The variable is considered to be an 'indexed' variable if the name starts with two
    /// underscores followed by a number. e.g. "__1".
    // TODO: Consider replacing this logic with `std::str::pattern::Pattern` when that API stabilizes
    pub fn is_indexed(&self) -> bool {
        match &self.name {
            VariableName::Named(name) => {
                name.starts_with("__")
                    && name
                        .find(char::is_numeric)
                        .map_or(false, |zero_based_position| zero_based_position == 2)
            }
            // Other kind of variables are never indexed
            _ => false,
        }
    }

    /// Returns `true` if the variable has a name, `false` otherwise.
    pub fn is_named(&self) -> bool {
        matches!(&self.name, VariableName::Named(_))
    }

    /// `true` if the Variable has a valid value, or an empty value.
    /// `false` if the Variable has a VariableValue::Error(_) value
    pub fn is_valid(&self) -> bool {
        self.value.is_valid()
    }

    /// Format the variable.
    fn formatted_variable_value(
        &self,
        variable_cache: &VariableCache,
        indentation: usize,
        show_name: bool,
    ) -> Option<String> {
        let type_name = self.type_name();

        if !self.value.is_empty() {
            // This is the end of the recursion where we already have a scalar
            // value for a variable and we can just move it up.
            let line_start = line_indent_string(indentation);
            return Some(if show_name {
                format!("{line_start}{}: {} = {}", self.name, type_name, self.value)
            } else {
                format!("{line_start}{}", self.value)
            });
        } else if matches!(
            self.name,
            VariableName::AnonymousNamespace | VariableName::Namespace(_)
        ) {
            // Namespaces do not have values, so we report no value up.
            // This will alow us to filter it out when we concatenate children.
            return None;
        }

        // Infer a human readable value using the available children of this variable.
        let children = &mut variable_cache.get_children(self.variable_key);
        let first_child = children.clone().next();

        // Make sure we can safely unwrap() children.
        Some(match self.type_name.inner() {
            VariableType::Pointer(_) => {
                format_pointer_value(variable_cache, indentation, first_child)
            }
            VariableType::Array { .. } => {
                format_array_value(variable_cache, indentation, children, &type_name)
            }
            VariableType::Struct(name) if name == "Some" || name == "Ok" || name == "Err" => {
                format_struct_value(variable_cache, indentation, children, &type_name)
            }
            _ if first_child.is_none() => {
                // This is a struct with no children, so just print the type name.
                // This is for example the None value of an Option or the empty type ().
                type_name
            }
            _ if matches!(
                self.name,
                VariableName::StaticScopeRoot
                    | VariableName::LocalScopeRoot
                    | VariableName::RegistersRoot
            ) =>
            {
                format_root_value(variable_cache, indentation, children, &type_name)
            }
            _ => format_default_value(
                variable_cache,
                indentation,
                &self.name,
                children,
                &type_name,
                show_name,
            ),
        })
    }

    /// Calculate the memory range that contains the value of this variable.
    ///
    /// If the location and/or byte size is not known, then return None.
    /// Note: We don't do any validation of the memory range here and leave it
    /// up to the caller to validate the memory ranges before attempting to read
    /// them.
    pub fn memory_range(&self) -> Option<Range<u64>> {
        let VariableLocation::Address(address) = self.memory_location else {
            return None;
        };

        self.byte_size.map(|byte_size| {
            if byte_size == 0 {
                address..address + 4
            } else {
                address..(address + byte_size)
            }
        })
    }
}

/// Format a pointer value
///
/// Formats the pointed to value and potential subsequent children as well.
fn format_pointer_value(
    variable_cache: &VariableCache,
    indentation: usize,
    first_child: Option<&Variable>,
) -> String {
    let line_start = line_indent_string(indentation);

    let value = if let Some(first_child) = first_child {
        first_child
            .formatted_variable_value(variable_cache, indentation + 1, true)
            .expect("a child. This is a bug. Please report it.")
    } else {
        "Unable to resolve referenced variable value".to_string()
    };

    format!("{line_start}{value}")
}

/// Format any array like value.
///
/// Recursively formats all child values.
fn format_array_value<'a>(
    variable_cache: &VariableCache,
    indentation: usize,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
    let line_start = line_indent_string(indentation);

    // Limit arrays to 10 elements
    const ARRAY_MAX_LENGTH: usize = 10;

    // If we at least ARRAY_MAX_LENGTH + 2 items in the iterator, cap at ARRAY_MAX_LENGTH.
    // If we have less, cap at the actual number of items.
    // This helps us to never write "and 1 more" with the reasoning that the space used for this
    // text, can be used for printing that one item.
    let count = children.clone().count();
    let take = if count > ARRAY_MAX_LENGTH + 1 {
        ARRAY_MAX_LENGTH
    } else {
        count
    };

    let children_values = children
        .by_ref()
        .take(take)
        .filter_map(|child| child.formatted_variable_value(variable_cache, indentation + 1, false))
        .join(",");

    let remainder = if count > ARRAY_MAX_LENGTH + 1 {
        format!(",\n{line_start}\t... and {} more", count - take)
    } else {
        String::new()
    };

    format!("{line_start}{type_name} = [{children_values}{remainder}{line_start}]")
}

/// Format any struct like value .
///
/// Recursively formats all child values.
fn format_struct_value<'a>(
    variable_cache: &VariableCache,
    indentation: usize,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
    let line_start = line_indent_string(indentation);

    // FIXME: this is not hit by any of the unwind tests, which is weird because
    // some of them contain `Some` structs.
    // Handle special structure types like the variant values of `Option<>` and `Result<>`
    let children_values = format_children_values(variable_cache, indentation, children, false);

    format!("{line_start}{type_name} = ({children_values})")
}

/// Format any root value.
///
/// Recursively formats all child values.
fn format_root_value<'a>(
    variable_cache: &VariableCache,
    indentation: usize,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
    let line_start = line_indent_string(indentation);

    let children_values = format_children_values(variable_cache, indentation, children, true);
    format!("{line_start}{type_name} {{{children_values}{line_start}}}")
}

/// Format any value that has no type that requires special handling.
///
/// Recursively formats all child values.
fn format_default_value<'a>(
    variable_cache: &VariableCache,
    indentation: usize,
    name: &VariableName,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &String,
    show_name: bool,
) -> String {
    let line_start = line_indent_string(indentation);

    // Find the first child of the structure if it exists.
    let child = children.clone().find(|v| v.is_named());

    // If we do not have children, exit early because we cannot print more specifics (children)
    // of this variable type. We instead print the empty type symbol.
    let Some(child) = child else {
        return "()".to_string();
    };

    let child_type_name = child.type_name();
    if child.is_indexed() {
        // Treat this structure as a tuple
        let children_values = format_children_values(variable_cache, indentation, children, false);
        let name = if show_name {
            format!("{name}: {type_name}({child_type_name}) = ")
        } else {
            String::new()
        };
        format!("{line_start}{name}{type_name}({children_values}{line_start})")
    } else {
        // Treat this structure as a `struct`
        let children_values = format_children_values(variable_cache, indentation, children, true);
        let name = if show_name {
            format!("{name}: {type_name} = ")
        } else {
            String::new()
        };
        format!("{line_start}{name}{type_name} {{{children_values}{line_start}}}")
    }
}

/// Concatenate all children values with a comma.
fn format_children_values<'a>(
    variable_cache: &VariableCache,
    indentation: usize,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    show_name: bool,
) -> String {
    children
        .filter_map(|child| {
            child.formatted_variable_value(variable_cache, indentation + 1, show_name)
        })
        .join(",")
}

/// Genarate a string that indents the line exactly the right amount.
/// Includes a newline at the start if the indentation is bigger than 0.
fn line_indent_string(indentation: usize) -> String {
    let line_feed = if indentation == 0 { "" } else { "\n" };
    format!("{line_feed}{:\t<indentation$}", "")
}
