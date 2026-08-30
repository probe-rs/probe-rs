use crate::{language::ProgrammingLanguage, unit_info::UnitInfo};

use super::*;
use gimli::{DebugInfoOffset, DwLang, UnitOffset};
use itertools::Itertools;
use probe_rs::RegisterValue;
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
    Artificial,
    /// Anonymous namespace
    AnonymousNamespace,
    /// A Namespace with a specific name
    Namespace(String),
    /// Variable with a specific name
    Named(String),
    /// Entry of an array or similar
    Indexed(u64),
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
            VariableName::Artificial => write!(f, "<artificial>"),
            VariableName::AnonymousNamespace => write!(f, "<anonymous_namespace>"),
            VariableName::Namespace(name) => name.fmt(f),
            VariableName::Named(name) => name.fmt(f),
            VariableName::Indexed(index) => write!(f, "__{index}"),
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
    ///   have enough information to expand it on demand. Except if they fall into one of the
    ///   special cases handled by [VariableNodeType::RecurseToBaseType]
    TypeOffset(DebugInfoOffset, UnitOffset),
    /// Use the `header_offset` and `entries_offset` as direct references for recursing the variable
    /// children.
    /// - Rule: All top level variables in a [StackFrame] are automatically deferred, i.e
    ///   [VariableName::LocalScopeRoot], [VariableName::RegistersRoot].
    DirectLookup(DebugInfoOffset, UnitOffset),
    /// Look up information from all compilation units. This is used to resolve static variables, so
    /// when [`VariableName::StaticScopeRoot`] is used.
    UnitsLookup,
    /// Use the `header_offset` and `type_offset` to resolve what a pointer points at.
    /// - Rule: Following a pointer is the only step of the walk that can return to a type it has
    ///   already expanded, so it is the only step that is deferred.
    /// - Rule: A pointer that holds no object, a null pointer, a dangling pointer, or a pointer to
    ///   a zero sized type, is not deferred.
    PointerTarget(DebugInfoOffset, UnitOffset),
    /// Sometimes it doesn't make sense to recurse the children of a specific node type
    /// - Rule: Pointers to `unit` datatypes WILL NOT BE resolved, because it doesn't make sense.
    /// - Rule: Once we determine that a variable can not be recursed further, we update the
    ///   variable_node_type to indicate that no further recursion is possible/required. This
    ///   can be because the variable is a 'base' data type, or because there was some kind of
    ///   error in processing the current node, so we don't want to incur cascading errors.
    // TODO: Find code instances where we use magic values (e.g. u32::MAX) and replace with DoNotRecurse logic if appropriate.
    DoNotRecurse,
    /// Unless otherwise specified, always recurse the children of every node until we get to the
    /// base data type.
    /// - Rule: (Default) Unless it is prevented by any of the other rules, we always recurse the
    ///   children of these variables.
    /// - Rule: Certain structured variables (e.g. `&str`, `Some`, `Ok`, `Err`, etc.) are set to
    ///   [VariableNodeType::RecurseToBaseType] to improve the debugger UX.
    /// - Rule: Pointers to `const` variables WILL ALWAYS BE recursed, because they provide
    ///   essential information, for example about the length of strings, or the size of
    ///   arrays.
    /// - Rule: Enumerated types WILL ALWAYS BE recursed, because we only ever want to see the
    ///   'active' child as the value.
    /// - Rule: For now, Array types WILL ALWAYS BE recursed. TODO: Evaluate if it is beneficial to
    ///   defer these.
    /// - Rule: Union types are deferred, like structured types.
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
            | VariableNodeType::UnitsLookup
            | VariableNodeType::PointerTarget(_, _) => true,
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
        match 1u128.checked_shl(self.length as u32) {
            Some(bit_above) => bit_above - 1,
            None => u128::MAX,
        }
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

/// How a [`VariableType`] name is formatted for the debugger UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeNameStyle {
    /// Crate and module path, plus generic arguments in the same style.
    Qualified,
    /// Ident plus generic arguments in the same style, with no path.
    Compact,
}

/// A type or const generic argument of a named type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum GenericArg {
    /// A type argument.
    Type(VariableType),
    /// A const generic value.
    Const(String),
}

impl GenericArg {
    fn display(&self, language: &dyn ProgrammingLanguage, style: TypeNameStyle) -> String {
        match self {
            GenericArg::Type(ty) => ty.display_name_with_style(language, style),
            GenericArg::Const(value) => value.clone(),
        }
    }
}

/// Ident, namespace, and generic arguments of a struct or enum.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct NamedType {
    /// The type ident, without a crate or module path.
    pub ident: Box<str>,
    /// Enclosing namespace segments, crate first.
    pub namespace: Box<[String]>,
    /// Generic arguments from DWARF template parameter DIEs.
    pub args: Box<[GenericArg]>,
}

impl NamedType {
    /// Last path segment of `ident`, without a generic argument list.
    pub fn ident_stem(&self) -> &str {
        self.ident
            .split_once('<')
            .map_or(self.ident.as_ref(), |(head, _)| head)
    }

    /// Build a named type from a DWARF `DW_AT_name` and namespace DIEs.
    pub(crate) fn from_dwarf(
        raw_name: String,
        namespace: Vec<String>,
        args: Vec<GenericArg>,
        language: &dyn ProgrammingLanguage,
    ) -> Self {
        let remainder = if namespace.is_empty() {
            raw_name
        } else {
            let prefix = namespace.join(language.type_path_separator());
            raw_name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_prefix(language.type_path_separator()))
                .map(str::to_string)
                .unwrap_or(raw_name)
        };

        if !args.is_empty() {
            let ident = remainder
                .split_once('<')
                .map(|(head, _)| head.to_string())
                .unwrap_or(remainder);
            return Self {
                ident: ident.into_boxed_str(),
                namespace: namespace.into_boxed_slice(),
                args: args.into_boxed_slice(),
            };
        }

        if let Some(VariableType::Struct(parsed) | VariableType::Enum(parsed)) =
            language.parse_type_name(&remainder)
        {
            return Self {
                ident: parsed.ident,
                namespace: if namespace.is_empty() {
                    parsed.namespace
                } else {
                    namespace.into_boxed_slice()
                },
                args: parsed.args,
            };
        }

        Self {
            ident: remainder.into_boxed_str(),
            namespace: namespace.into_boxed_slice(),
            args: args.into_boxed_slice(),
        }
    }

    pub(crate) fn display(
        &self,
        language: &dyn ProgrammingLanguage,
        style: TypeNameStyle,
    ) -> String {
        let args: Vec<String> = self
            .args
            .iter()
            .map(|arg| arg.display(language, style))
            .collect();
        let head = language
            .format_named_head(&self.ident, &args)
            .unwrap_or_else(|| language.format_generic_type(&self.ident, &args));
        match style {
            TypeNameStyle::Compact => head,
            TypeNameStyle::Qualified
                if self.namespace.is_empty() || !language.is_path_ident(&self.ident) =>
            {
                head
            }
            TypeNameStyle::Qualified => {
                let separator = language.type_path_separator();
                format!("{}{separator}{head}", self.namespace.join(separator))
            }
        }
    }
}

impl From<&str> for NamedType {
    fn from(ident: &str) -> Self {
        Self::from(ident.to_string())
    }
}

impl From<String> for NamedType {
    fn from(ident: String) -> Self {
        Self {
            ident: ident.into_boxed_str(),
            namespace: Vec::new().into_boxed_slice(),
            args: Vec::new().into_boxed_slice(),
        }
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
    Struct(NamedType),
    /// A Rust enum.
    Enum(NamedType),
    /// Namespace refers to the path that qualifies a variable. e.g. "std::string" is the namespace
    /// for the struct "String"
    Namespace,
    /// A Pointer is a variable that contains a reference to another variable, and the type of the
    /// referenced variable may not be known until the reference has been resolved.
    Pointer(Option<Box<VariableType>>),
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
        self.ident()
            .is_some_and(|ident| ident.starts_with("PhantomData"))
    }

    /// The ident of a named type, without a crate or module path.
    pub fn ident(&self) -> Option<&str> {
        match self.inner() {
            VariableType::Base(name) | VariableType::Other(name) => Some(name.as_str()),
            VariableType::Struct(name) | VariableType::Enum(name) => Some(name.ident_stem()),
            VariableType::Pointer(Some(ty)) => ty.ident(),
            _ => None,
        }
    }

    /// Named type data for a struct or enum.
    pub fn named(&self) -> Option<&NamedType> {
        match self.inner() {
            VariableType::Struct(name) | VariableType::Enum(name) => Some(name),
            _ => None,
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
        self.display_name_with_style(language, TypeNameStyle::Qualified)
    }

    pub(crate) fn display_name_with_style(
        &self,
        language: &dyn ProgrammingLanguage,
        style: TypeNameStyle,
    ) -> String {
        match self {
            VariableType::Modified(Modifier::Typedef(name), _) => name.clone(),
            VariableType::Modified(modifier, ty) => {
                language.modified_type_name(modifier, &ty.display_name_with_style(language, style))
            }

            VariableType::Array {
                item_type_name,
                count,
            } => language.format_array_type(
                // In case the compiler points at a modified item type (e.g. const), skip the
                // modifier.
                &item_type_name
                    .skip_modifiers()
                    .display_name_with_style(language, style),
                *count,
            ),

            VariableType::Bitfield(bitfield, name) => language
                .format_bitfield_type(&name.display_name_with_style(language, style), *bitfield),

            VariableType::Struct(name) | VariableType::Enum(name) => name.display(language, style),

            _ => self.type_name_with_style(language, style),
        }
    }

    fn type_name_with_style(
        &self,
        language: &dyn ProgrammingLanguage,
        style: TypeNameStyle,
    ) -> String {
        match self {
            VariableType::Base(name) | VariableType::Other(name) => name.clone(),
            VariableType::Struct(name) | VariableType::Enum(name) => name.display(language, style),
            VariableType::Namespace => "namespace".to_string(),
            VariableType::Unknown => "<unknown>".to_string(),
            VariableType::Pointer(pointee) => match pointee {
                Some(ty) => {
                    let inner = ty.display_name_with_style(language, style);
                    if inner.starts_with(['*', '&']) {
                        inner
                    } else {
                        language.format_pointer_type(Some(&inner))
                    }
                }
                None => language.format_pointer_type(None),
            },
            VariableType::Array {
                item_type_name,
                count,
            } => language.format_array_type(
                &item_type_name.type_name_with_style(language, style),
                *count,
            ),
            VariableType::Bitfield(_, ty) | VariableType::Modified(_, ty) => {
                ty.type_name_with_style(language, style)
            }
        }
    }
}

/// Location of a variable
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// The variable is stored in a register, and the value is read from there.
    RegisterValue(RegisterValue),
    /// The value is assembled from more than one place, or from part of a place. The pieces are
    /// ordered from the least significant bits of the value to the most significant bits.
    Composite(Vec<LocationPiece>),
    /// There was an error evaluating the variable location.
    Error(String),
    /// Support for handling the location of this variable is not (yet) implemented.
    Unsupported(String),
}

/// One piece of the value of a variable. See section '2.6.1.2 Composite Location Descriptions' of
/// the DWARF 5 specification.
#[derive(Debug, Clone, PartialEq)]
pub struct LocationPiece {
    /// The place that holds the bits of this piece.
    pub source: PieceSource,
    /// The offset of the piece inside the source, in bits.
    pub bit_offset: u64,
    /// The size of the piece in bits. `None` means that the piece holds all of the value.
    pub bit_size: Option<u64>,
}

/// The place that holds the bits of a [`LocationPiece`].
#[derive(Debug, Clone, PartialEq)]
pub enum PieceSource {
    /// The bits are in target memory, at this address.
    Address(u64),
    /// The bits are in a register, which holds this value.
    Register(RegisterValue),
    /// The bits are a constant that the debug info holds.
    Implicit(Vec<u8>),
    /// The piece has no place, because the compiler optimized it away.
    Empty,
}

impl PieceSource {
    /// Read `bits` bits, starting `bit_offset` bits into the source. The result holds the bits in
    /// little endian order, and the first bit of the piece is the least significant bit.
    fn read_bits(
        &self,
        bit_offset: u64,
        bits: u64,
        memory: &mut dyn MemoryInterface,
    ) -> Result<Vec<u8>, DebugError> {
        match self {
            PieceSource::Address(address) => {
                let Some(address) = address.checked_add(bit_offset / 8) else {
                    return Err(DebugError::WarnAndContinue {
                        message: "Overflow calculating the address of a value piece".to_string(),
                    });
                };

                let mut buffer = vec![0u8; (bit_offset % 8 + bits).div_ceil(8) as usize];
                memory.read(address, &mut buffer)?;

                Ok(extract_bits(&buffer, bit_offset % 8, bits))
            }
            PieceSource::Register(value) => {
                let value = TryInto::<u128>::try_into(*value)?;
                Ok(extract_bits(&value.to_le_bytes(), bit_offset, bits))
            }
            PieceSource::Implicit(bytes) => Ok(extract_bits(bytes, bit_offset, bits)),
            PieceSource::Empty => Ok(vec![0; bits.div_ceil(8) as usize]),
        }
    }
}

/// Take `bits` bits of a value that pieces hold, starting `bit_offset` bits into the value.
fn slice_pieces(pieces: &[LocationPiece], bit_offset: u64, bits: Option<u64>) -> VariableLocation {
    let mut skip = bit_offset;
    let mut remaining = bits;
    let mut result = Vec::new();

    for piece in pieces {
        let Some(size) = piece.bit_size else {
            // The piece holds all of the value, so the offset applies to the piece itself.
            result.push(LocationPiece {
                source: piece.source.clone(),
                bit_offset: piece.bit_offset + skip,
                bit_size: remaining,
            });
            break;
        };

        if skip >= size {
            skip -= size;
            continue;
        }

        let available = size - skip;
        let take = remaining.map_or(available, |remaining| remaining.min(available));
        if take == 0 {
            break;
        }

        result.push(LocationPiece {
            source: piece.source.clone(),
            bit_offset: piece.bit_offset + skip,
            bit_size: Some(take),
        });

        skip = 0;
        if let Some(remaining) = remaining.as_mut() {
            *remaining -= take;
            if *remaining == 0 {
                break;
            }
        }
    }

    if result
        .iter()
        .all(|piece| piece.source == PieceSource::Empty)
    {
        // The pieces hold no bits, or the compiler optimized all of the bits away.
        return VariableLocation::Unavailable;
    }

    match &result[..] {
        // A value that memory holds as a whole number of bytes keeps its address, so that the
        // debugger can still show the memory of the value and follow it as a pointer.
        [
            LocationPiece {
                source: PieceSource::Address(address),
                bit_offset,
                bit_size,
            },
        ] if bit_offset % 8 == 0 && bit_size.is_none_or(|bits| bits % 8 == 0) => {
            match address.checked_add(bit_offset / 8) {
                Some(address) => VariableLocation::Address(address),
                None => {
                    VariableLocation::Error("Overflow calculating variable address".to_string())
                }
            }
        }
        // A member that occupies a whole register keeps the register location, so that a pointer
        // that lives in a register still prints as the register value.
        [
            LocationPiece {
                source: PieceSource::Register(value),
                bit_offset: 0,
                bit_size,
            },
        ] if bit_size.is_none_or(|bits| bits == register_bit_size(*value)) => {
            VariableLocation::RegisterValue(*value)
        }
        _pieces => VariableLocation::Composite(result),
    }
}

fn register_bit_size(value: RegisterValue) -> u64 {
    match value {
        RegisterValue::U32(_) => 32,
        RegisterValue::U64(_) => 64,
        RegisterValue::U128(_) => 128,
    }
}

fn implicit_byte_size(pieces: &[LocationPiece]) -> Option<usize> {
    let [
        LocationPiece {
            source: PieceSource::Implicit(bytes),
            bit_size,
            ..
        },
    ] = pieces
    else {
        return None;
    };

    let size = bit_size.map_or(bytes.len() as u64, |bits| bits.div_ceil(8));
    (1..=8).contains(&size).then_some(size as usize)
}

/// Copy `bits` bits out of a little endian buffer, starting at `bit_offset`.
fn extract_bits(source: &[u8], bit_offset: u64, bits: u64) -> Vec<u8> {
    let mut destination = vec![0u8; bits.div_ceil(8) as usize];
    insert_bits(source, bit_offset, &mut destination, 0, bits);
    destination
}

/// Copy `bits` bits from `bit_offset` in a little endian buffer to `destination_offset` in another.
/// Bits that the source does not hold stay unchanged in the destination.
fn insert_bits(
    source: &[u8],
    bit_offset: u64,
    destination: &mut [u8],
    destination_offset: u64,
    bits: u64,
) {
    for bit in 0..bits {
        let from = bit_offset + bit;
        let to = destination_offset + bit;

        let (Some(source_byte), Some(destination_byte)) = (
            source.get((from / 8) as usize),
            destination.get_mut((to / 8) as usize),
        ) else {
            return;
        };

        let mask = 1 << (to % 8);
        if source_byte >> (from % 8) & 1 == 1 {
            *destination_byte |= mask;
        } else {
            *destination_byte &= !mask;
        }
    }
}

impl VariableLocation {
    /// Return the memory address, if available. Otherwise an error is returned.
    ///
    /// A register location holds the value, not an address of the value.
    pub fn memory_address(&self) -> Result<u64, DebugError> {
        match self {
            VariableLocation::Address(address) => Ok(*address),
            VariableLocation::RegisterValue(_) => Err(DebugError::WarnAndContinue {
                message: "The value is in a register and has no memory address".to_string(),
            }),
            VariableLocation::Error(error) => Err(DebugError::WarnAndContinue {
                message: error.clone(),
            }),
            other => Err(DebugError::WarnAndContinue {
                message: format!("Variable does not have a memory location: location={other:?}"),
            }),
        }
    }

    /// The address of the value, if target memory holds the value.
    pub fn address(&self) -> Option<u64> {
        match self {
            VariableLocation::Address(address) => Some(*address),
            _other => None,
        }
    }

    /// Check if the location is valid, ie. not an error, unsupported, or unavailable.
    pub fn valid(&self) -> bool {
        match self {
            VariableLocation::Address(_)
            | VariableLocation::RegisterValue(_)
            | VariableLocation::Composite(_)
            | VariableLocation::Value
            | VariableLocation::Unknown => true,
            _other => false,
        }
    }

    /// Target storage holds this value, so members can be expanded.
    pub fn holds_value(&self) -> bool {
        matches!(
            self,
            VariableLocation::Address(_)
                | VariableLocation::RegisterValue(_)
                | VariableLocation::Composite(_)
                | VariableLocation::Value
        )
    }

    /// The location of a value of `byte_size` bytes, `byte_offset` bytes into this location.
    pub fn offset_by(&self, byte_offset: u64, byte_size: Option<u64>) -> VariableLocation {
        let bit_offset = byte_offset.saturating_mul(8);
        let bit_size = byte_size.map(|byte_size| byte_size.saturating_mul(8));

        match self {
            VariableLocation::Address(address) => match address.checked_add(byte_offset) {
                Some(address) => VariableLocation::Address(address),
                None => {
                    VariableLocation::Error("Overflow calculating variable address".to_string())
                }
            },
            VariableLocation::RegisterValue(value) => slice_pieces(
                &[LocationPiece {
                    source: PieceSource::Register(*value),
                    bit_offset: 0,
                    // The register holds all of the value, so a value that starts after the
                    // register has no location.
                    bit_size: Some(register_bit_size(*value)),
                }],
                bit_offset,
                bit_size,
            ),
            VariableLocation::Composite(pieces) => slice_pieces(pieces, bit_offset, bit_size),
            other => other.clone(),
        }
    }

    /// Read the value that this location holds into `buffer`, in little endian order.
    ///
    /// A value that is shorter than the buffer leaves the remaining bytes unchanged.
    pub fn read(
        &self,
        buffer: &mut [u8],
        memory: &mut dyn MemoryInterface,
    ) -> Result<(), DebugError> {
        match self {
            VariableLocation::Address(address) => memory.read(*address, buffer)?,
            VariableLocation::RegisterValue(value) => {
                let value = TryInto::<u128>::try_into(*value)?.to_le_bytes();
                let bytes = buffer.len().min(value.len());
                buffer[..bytes].copy_from_slice(&value[..bytes]);
            }
            VariableLocation::Composite(pieces) => {
                let capacity = (buffer.len() * 8) as u64;
                let mut offset = 0;

                for piece in pieces {
                    let available = capacity - offset;
                    if available == 0 {
                        break;
                    }

                    if piece.source == PieceSource::Empty {
                        return Err(DebugError::WarnAndContinue {
                            message: "The compiler optimized a part of this value away".to_string(),
                        });
                    }

                    let bits = piece.bit_size.unwrap_or(available).min(available);
                    let source = piece.source.read_bits(piece.bit_offset, bits, memory)?;

                    insert_bits(&source, 0, buffer, offset, bits);
                    offset += bits;
                }
            }
            VariableLocation::Error(error) => {
                return Err(DebugError::WarnAndContinue {
                    message: error.clone(),
                });
            }
            other => {
                return Err(DebugError::WarnAndContinue {
                    message: format!("Variable does not have a readable location: {other}"),
                });
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for VariableLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableLocation::Unknown => "<unknown value>".fmt(f),
            VariableLocation::Unavailable => "<value not available>".fmt(f),
            VariableLocation::Address(address) => {
                write!(f, "{address:#010X}")
            }
            VariableLocation::RegisterValue(address) => match address {
                RegisterValue::U32(value) => write!(f, "{value:#010X}"),
                RegisterValue::U64(value) => write!(f, "{value:#018X}"),
                RegisterValue::U128(value) => write!(f, "{value:#034X}"),
            },
            VariableLocation::Composite(_) => "<composite value>".fmt(f),
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
#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    /// Every variable must have a unique key value assigned to it.
    /// The value will be zero until it is stored in VariableCache, at which time its value will be
    /// set to the same as the VariableCache::variable_cache_key
    pub(super) variable_key: ObjectRef,
    /// The offset to the variable's type information, relative to the debug info section.
    pub(crate) type_node_offset: Option<DebugInfoOffset>,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache.
    pub parent_key: ObjectRef,
    /// The variable name refers to the name of any of the types of values described in the [VariableCache]
    pub name: VariableName,

    /// Linkage name of the variable. Multiple variables with the same name could exist,
    /// this is used to distinguish between them.
    pub(crate) linkage_name: Option<String>,

    /// Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    pub(super) value: VariableValue,
    /// The source location of the declaration of this variable, if available.
    pub source_location: Option<SourceLocation>,
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
    /// The address size of the compilation unit, in bytes.
    address_size: Option<u8>,
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
            type_node_offset: None,
            variable_key: Default::default(),
            parent_key: Default::default(),
            name: Default::default(),
            linkage_name: None,
            value: Default::default(),
            source_location: None,
            type_name: Default::default(),
            variable_node_type: Default::default(),
            memory_location: Default::default(),
            byte_size: None,
            address_size: unit_info.map(|info| info.unit.encoding().address_size),
            role: Default::default(),
        }
    }

    /// Returns the readable name of the variable type.
    pub fn type_name(&self) -> String {
        self.type_name
            .display_name(language::from_dwarf(self.language).as_ref())
    }

    /// Returns a short type name for use inside a variable value.
    pub fn compact_type_name(&self) -> String {
        self.type_name.display_name_with_style(
            language::from_dwarf(self.language).as_ref(),
            TypeNameStyle::Compact,
        )
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
            self.memory_location =
                VariableLocation::Error("Failed to resolve variable value".to_string());
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
            return format!("{}", self.value);
        }

        if matches!(
            self.name,
            VariableName::AnonymousNamespace | VariableName::Namespace(_)
        ) {
            // Namespaces do not have values
            return String::new();
        }

        // We need to construct a 'human readable' value using `fmt::Display` to represent the
        // values of complex types and pointers.
        if variable_cache.has_children(self) {
            self.formatted_variable_value(variable_cache, false)
                .unwrap_or_default()
        } else if matches!(self.type_name, VariableType::Array { count: 0, .. }) {
            // An empty array has no bytes, so it needs no location.
            self.formatted_variable_value(variable_cache, false)
                .unwrap_or_default()
        } else if self.type_name == VariableType::Unknown || !self.memory_location.holds_value() {
            if self.variable_node_type.is_deferred() {
                // When we will do a lazy-load of variable children, and they have not yet been
                // requested by the user, just display the type_name as the value
                self.compact_type_name()
            } else if !self.memory_location.holds_value() {
                // The location explains why the variable has no value.
                self.memory_location.to_string()
            } else {
                // This condition should only be true for intermediate nodes
                // from DWARF. These should not show up in the final
                // `VariableCache`. If a user sees this error, then there is
                // a logic problem in the stack unwind
                "Error: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
            }
        } else if matches!(self.type_name, VariableType::Struct(ref name) if name.ident_stem() == "None")
        {
            "None".to_string()
        } else {
            format!(
                "Unimplemented: Get value of type {:?} of ({:?} bytes) at location {}",
                self.type_name, self.byte_size, self.memory_location
            )
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

        if matches!(self.type_name, VariableType::Pointer(_)) {
            // The value of a pointer is the address that it holds, not the place that holds the
            // pointer.
            let value = if let Some(location) =
                self.pointer_target(memory).map(VariableLocation::Address)
            {
                format!("{} @ {location}", self.compact_type_name())
            } else {
                self.compact_type_name()
            };
            self.value = VariableValue::Valid(value);
            return;
        }

        if self.variable_node_type.is_deferred() {
            // And we have not previously assigned the value, then assign the type and address as
            // the value.
            self.value = VariableValue::Valid(format!(
                "{} @ {}",
                self.compact_type_name(),
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

    /// The integer that this variable holds, if the location of the variable can be read as an
    /// integer of at most 8 bytes. A pointer holds the address that it refers to.
    pub(crate) fn integer_value(&self, memory: &mut dyn MemoryInterface) -> Option<u64> {
        if matches!(self.type_name.inner(), VariableType::Pointer(_)) {
            return self.pointer_target(memory);
        }

        if let VariableValue::Valid(value) = &self.value
            && let Ok(value) = value.parse()
        {
            return Some(value);
        }

        let byte_size = self.byte_size.filter(|size| (1..=8).contains(size))? as usize;
        let mut buffer = [0u8; 8];
        self.memory_location
            .read(&mut buffer[..byte_size], memory)
            .ok()?;
        Some(u64::from_le_bytes(buffer))
    }

    /// The address that a pointer holds, if the location of the pointer can be read.
    fn pointer_target(&self, memory: &mut dyn MemoryInterface) -> Option<u64> {
        let byte_size = self.pointer_byte_size()?;
        let mut buffer = [0u8; 8];

        self.memory_location
            .read(&mut buffer[..byte_size], memory)
            .ok()?;

        Some(u64::from_le_bytes(buffer))
    }

    fn pointer_byte_size(&self) -> Option<usize> {
        self.byte_size
            .or(self.address_size.map(u64::from))
            .filter(|size| (1..=8).contains(size))
            .map(|size| size as usize)
            .or_else(|| match &self.memory_location {
                VariableLocation::RegisterValue(value) => {
                    Some((register_bit_size(*value) / 8) as usize)
                }
                VariableLocation::Composite(pieces) => implicit_byte_size(pieces),
                _ => None,
            })
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
                        .is_some_and(|zero_based_position| zero_based_position == 2)
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
        show_name: bool,
    ) -> Option<String> {
        let type_name = self.compact_type_name();

        if !self.value.is_empty() {
            // This is the end of the recursion where we already have a scalar
            // value for a variable and we can just move it up.
            return Some(if show_name {
                format!("{} = {}", self.name, self.value)
            } else {
                format!("{}", self.value)
            });
        } else if matches!(
            self.name,
            VariableName::AnonymousNamespace | VariableName::Namespace(_)
        ) {
            // Namespaces do not have values, so we report no value up.
            // This will allow us to filter it out when we concatenate children.
            return None;
        }

        // Infer a human readable value using the available children of this variable.
        let children = &mut variable_cache.get_children(self.variable_key);
        let first_child = children.clone().next();

        // Make sure we can safely unwrap() children.
        Some(match self.type_name.inner() {
            VariableType::Pointer(_) => format_pointer_value(variable_cache, first_child),
            VariableType::Array { .. } => format_array_value(variable_cache, children, &type_name),
            VariableType::Struct(name) if matches!(name.ident_stem(), "Some" | "Ok" | "Err") => {
                format_struct_value(variable_cache, children, &type_name)
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
                format_root_value(variable_cache, children, &type_name)
            }
            _ => format_default_value(variable_cache, &self.name, children, &type_name, show_name),
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
fn format_pointer_value(variable_cache: &VariableCache, first_child: Option<&Variable>) -> String {
    if let Some(first_child) = first_child {
        first_child
            .formatted_variable_value(variable_cache, true)
            .expect("a child. This is a bug. Please report it.")
    } else {
        "Unable to resolve referenced variable value".to_string()
    }
}

/// Format any array like value.
///
/// Recursively formats all child values.
fn format_array_value<'a>(
    variable_cache: &VariableCache,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
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
        .filter_map(|child| child.formatted_variable_value(variable_cache, false))
        .join(", ");

    let remainder = if count > ARRAY_MAX_LENGTH + 1 {
        format!(", ... and {} more", count - take)
    } else {
        String::new()
    };

    format!("{type_name} = [{children_values}{remainder}]")
}

/// Format any struct like value .
///
/// Recursively formats all child values.
fn format_struct_value<'a>(
    variable_cache: &VariableCache,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
    // FIXME: this is not hit by any of the unwind tests, which is weird because
    // some of them contain `Some` structs.
    // Handle special structure types like the variant values of `Option<>` and `Result<>`
    let children_values = format_children_values(variable_cache, children, false);

    format!("{type_name} = ({children_values})")
}

/// Format any root value.
///
/// Recursively formats all child values.
fn format_root_value<'a>(
    variable_cache: &VariableCache,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &str,
) -> String {
    format!(
        "{type_name} {}",
        brace_list(&format_children_values(variable_cache, children, true))
    )
}

/// Format any value that has no type that requires special handling.
///
/// Recursively formats all child values.
fn format_default_value<'a>(
    variable_cache: &VariableCache,
    name: &VariableName,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    type_name: &String,
    show_name: bool,
) -> String {
    // Find the first child of the structure if it exists.
    let child = children.clone().find(|v| v.is_named());

    // If we do not have children, exit early because we cannot print more specifics (children)
    // of this variable type. We instead print the empty type symbol.
    let Some(child) = child else {
        return "()".to_string();
    };

    let child_type_name = child.compact_type_name();
    if child.is_indexed() {
        // Treat this structure as a tuple
        let children_values = format_children_values(variable_cache, children, false);
        let name = if show_name {
            format!("{name}: {type_name}({child_type_name}) = ")
        } else {
            String::new()
        };
        format!("{name}{type_name}({children_values})")
    } else {
        // Treat this structure as a `struct`
        let children_values = format_children_values(variable_cache, children, true);
        let name = if show_name {
            format!("{name}: {type_name} = ")
        } else {
            String::new()
        };
        format!("{name}{type_name} {}", brace_list(&children_values))
    }
}

/// Concatenate all children values with a comma.
fn format_children_values<'a>(
    variable_cache: &VariableCache,
    children: &mut (impl Iterator<Item = &'a Variable> + Clone),
    show_name: bool,
) -> String {
    children
        .filter_map(|child| child.formatted_variable_value(variable_cache, show_name))
        .join(", ")
}

fn brace_list(inner: &str) -> String {
    if inner.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {inner} }}")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use probe_rs::test::MockMemory;

    /// Memory that holds the byte `index` at address `index`.
    fn memory() -> MockMemory {
        let mut memory = MockMemory::new();
        memory.add_range(0, (0..=u8::MAX).collect());
        memory
    }

    fn piece(source: PieceSource, bit_offset: u64, bit_size: Option<u64>) -> LocationPiece {
        LocationPiece {
            source,
            bit_offset,
            bit_size,
        }
    }

    fn read(location: &VariableLocation, byte_size: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; byte_size];
        location.read(&mut buffer, &mut memory()).unwrap();
        buffer
    }

    #[test]
    fn a_value_in_memory_reads_the_bytes_at_the_address() {
        let location = VariableLocation::Address(4);

        assert_eq!(read(&location, 4), vec![4, 5, 6, 7]);
    }

    #[test]
    fn a_value_in_a_register_reads_the_least_significant_bytes() {
        let location = VariableLocation::RegisterValue(RegisterValue::U32(0xAABB_CCDD));

        assert_eq!(read(&location, 2), vec![0xDD, 0xCC]);
    }

    #[test]
    fn a_composite_value_appends_the_pieces_from_the_least_significant_bits() {
        // The low half comes from a register, the high half from memory.
        let location = VariableLocation::Composite(vec![
            piece(
                PieceSource::Register(RegisterValue::U32(0x0000_1234)),
                0,
                Some(16),
            ),
            piece(PieceSource::Address(0x10), 0, Some(16)),
        ]);

        assert_eq!(read(&location, 4), vec![0x34, 0x12, 0x10, 0x11]);
    }

    #[test]
    fn a_piece_reads_from_its_bit_offset() {
        // The second byte of the register, and the second byte in memory.
        let location = VariableLocation::Composite(vec![
            piece(
                PieceSource::Register(RegisterValue::U32(0xAABB_CCDD)),
                8,
                Some(8),
            ),
            piece(PieceSource::Address(0x20), 8, Some(8)),
        ]);

        assert_eq!(read(&location, 2), vec![0xCC, 0x21]);
    }

    #[test]
    fn pieces_that_are_not_a_whole_number_of_bytes_join_without_a_gap() {
        // Four bits of 0b1010, then four bits of 0b0011, then a full byte.
        let location = VariableLocation::Composite(vec![
            piece(PieceSource::Implicit(vec![0b1010]), 0, Some(4)),
            piece(PieceSource::Implicit(vec![0b0011]), 0, Some(4)),
            piece(PieceSource::Implicit(vec![0xEF]), 0, Some(8)),
        ]);

        assert_eq!(read(&location, 2), vec![0b0011_1010, 0xEF]);
    }

    #[test]
    fn a_piece_that_crosses_a_byte_boundary_reads_the_bits_of_both_bytes() {
        // Twelve bits, starting at bit 4 of the byte at address 0x30.
        let location =
            VariableLocation::Composite(vec![piece(PieceSource::Address(0x30), 4, Some(12))]);

        // 0x30 holds 0x30 and 0x31 holds 0x31, so the bits are 0x1 followed by 0x31.
        assert_eq!(read(&location, 2), vec![0x13, 0x03]);
    }

    #[test]
    fn a_value_with_a_piece_that_the_compiler_optimized_away_cannot_be_read() {
        let location = VariableLocation::Composite(vec![
            piece(PieceSource::Empty, 0, Some(8)),
            piece(PieceSource::Implicit(vec![0xAB]), 0, Some(8)),
        ]);

        assert!(location.read(&mut [0u8; 2], &mut memory()).is_err());
    }

    #[test]
    fn a_value_that_the_compiler_optimized_away_has_no_location() {
        let location = VariableLocation::Composite(vec![
            piece(PieceSource::Empty, 0, Some(32)),
            piece(PieceSource::Implicit(vec![0xAB; 4]), 0, Some(32)),
        ]);

        assert_eq!(
            location.offset_by(0, Some(4)),
            VariableLocation::Unavailable
        );
        assert_eq!(
            location.offset_by(4, Some(4)),
            VariableLocation::Composite(vec![piece(
                PieceSource::Implicit(vec![0xAB; 4]),
                0,
                Some(32)
            )])
        );
    }

    #[test]
    fn a_location_without_a_value_cannot_be_read() {
        assert!(
            VariableLocation::Unavailable
                .read(&mut [0u8], &mut memory())
                .is_err()
        );
    }

    #[test]
    fn a_pointer_that_lives_in_a_register_resolves_to_the_register_value_without_a_memory_access() {
        let location = VariableLocation::RegisterValue(RegisterValue::U32(0x3FCD_C3B0));
        let mut buffer = [0u8; 8];
        let address_size = 4;

        location
            .read(&mut buffer[..address_size], &mut MockMemory::new())
            .unwrap();

        assert_eq!(u64::from_le_bytes(buffer), 0x3FCD_C3B0);
    }

    #[test]
    fn a_member_of_a_register_held_struct_reads_the_bits_of_the_register_at_the_offset_of_the_member()
     {
        let location = VariableLocation::RegisterValue(RegisterValue::U32(0xAABB_CCDD));
        let member = location.offset_by(2, Some(2));

        assert_eq!(
            member,
            VariableLocation::Composite(vec![piece(
                PieceSource::Register(RegisterValue::U32(0xAABB_CCDD)),
                16,
                Some(16)
            )])
        );

        let mut buffer = [0u8; 2];
        member.read(&mut buffer, &mut MockMemory::new()).unwrap();
        assert_eq!(buffer, [0xBB, 0xAA]);
    }

    #[test]
    fn a_member_that_starts_after_the_register_has_no_location() {
        let location = VariableLocation::RegisterValue(RegisterValue::U32(0xAABB_CCDD));

        assert_eq!(
            location.offset_by(4, Some(4)),
            VariableLocation::Unavailable
        );
    }

    #[test]
    fn a_write_to_a_variable_that_lives_in_a_register_fails() {
        let mut variable = Variable::new(None);
        variable.name = VariableName::Named("x".to_string());
        variable.type_name = VariableType::Base("u32".to_string());
        variable.memory_location = VariableLocation::RegisterValue(RegisterValue::U32(0x2000_0000));
        variable.value = VariableValue::Valid("1".to_string());
        variable.byte_size = Some(4);

        let mut cache = crate::VariableCache::new_static_cache();
        let error = variable
            .update_value(&mut MockMemory::new(), &mut cache, "2".to_string())
            .expect_err("a register value cannot be updated");

        assert!(
            error.to_string().contains("register"),
            "the error must name the register as the reason: {error}"
        );
    }

    fn rust_lang() -> crate::language::rust::Rust {
        crate::language::rust::Rust
    }

    #[test]
    fn a_named_type_strips_the_dwarf_namespace_prefix() {
        let language = rust_lang();
        let string = NamedType::from_dwarf(
            "alloc::string::String".to_string(),
            vec!["alloc".to_string(), "string".to_string()],
            Vec::new(),
            &language,
        );
        let option = NamedType::from_dwarf(
            "core::option::Option<alloc::string::String>".to_string(),
            vec!["core".to_string(), "option".to_string()],
            vec![GenericArg::Type(VariableType::Struct(string))],
            &language,
        );

        assert_eq!(
            option.display(&language, TypeNameStyle::Compact),
            "Option<String>"
        );
        assert_eq!(
            option.display(&language, TypeNameStyle::Qualified),
            "core::option::Option<alloc::string::String>"
        );
    }

    #[test]
    fn a_named_type_parses_generic_arguments_when_dwarf_has_no_template_parameters() {
        let language = rust_lang();
        let option = NamedType::from_dwarf(
            "Option<esp_hal::clocks::XtalClkConfig>".to_string(),
            vec!["core".to_string(), "option".to_string()],
            Vec::new(),
            &language,
        );

        assert_eq!(option.ident.as_ref(), "Option");
        assert_eq!(option.ident_stem(), "Option");
        assert_eq!(
            option.display(&language, TypeNameStyle::Compact),
            "Option<XtalClkConfig>"
        );
        assert_eq!(
            option.display(&language, TypeNameStyle::Qualified),
            "core::option::Option<esp_hal::clocks::XtalClkConfig>"
        );
    }
}
