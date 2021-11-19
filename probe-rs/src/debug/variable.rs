use num_traits::Zero;
use thousands::Separable;

use super::*;
use std::{convert::TryInto, fmt};

/// VariableKind is a tag used to differentiate the nature of a variable. The DAP protocol requires a differentiation between 'Named' and 'Indexed'. We've added some flags to control when variables require unique handling or decoding the value during runtimeprocessing.
#[derive(Debug, Clone, PartialEq)]
pub enum VariableKind {
    /// An Indexed variable (bound to an ordinal position), such as the sequenced members of an Array or Vector
    Indexed,
    /// A variable that is identified by it's name, and is not bound to a specific ordinal position.
    Named,
    /// A variable that points to another variable
    Pointer,
    /// A variable that is the target of a pointer variable
    Referenced,
    /// As the default, his should never be the final value for a Variable
    Undetermined,
}
impl Default for VariableKind {
    fn default() -> Self {
        VariableKind::Undetermined
    }
}

/// VariableInclusion is a tag used to control when a variable should be included in the final result tree, or if it is simply an artifact of decoding the DWARF structure
#[derive(Debug, Clone, PartialEq)]
pub enum VariableInclusion {
    /// Exclude nodes that are encountered as 'structural' during the evaluation of other variables. e.g. DW_AT_artificial
    Exclude,
    /// When a variable is set to Include, all parents in the tree will be included also
    Include,
    /// As the default, this should never be the final value for a Variable
    Undetermined,
}
impl Default for VariableInclusion {
    fn default() -> Self {
        VariableInclusion::Undetermined
    }
}

/// Define the role that a variable plays in a Variant relationship. See section '5.7.10 Variant Entries' of the DWARF 5 specification
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VariantRole {
    /// A (parent) Variable that can have any number of Variant's as it's value
    VariantPart(u64),
    /// A (child) Variable that defines one of many possible types to hold the current value of a VariantPart.
    Variant(u64),
    /// This variable doesn't play a role in a Variant relationship
    NonVariant,
}

impl Default for VariantRole {
    fn default() -> Self {
        VariantRole::NonVariant
    }
}

#[derive(Debug, Default, Clone)]
pub struct Variable {
    pub name: String,
    value: String,
    pub file: String,
    pub line: u64,
    pub type_name: String,
    /// The starting location/address in memory where this Variable's value is stored.
    pub memory_location: u64,
    pub byte_size: u64,
    /// If  this is a subrange (array, vector, etc.), is the ordinal position of this variable in that range
    pub(crate) member_index: Option<i64>,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the lower bound.
    pub(crate) range_lower_bound: i64,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the the upper bound of the range.
    pub(crate) range_upper_bound: i64,
    pub kind: VariableKind,
    pub role: VariantRole,
    pub(crate) inclusion: VariableInclusion,
    pub children: Option<Vec<Variable>>,
}

impl Variable {
    pub fn new() -> Variable {
        Variable {
            name: String::new(),
            value: String::new(),
            file: String::new(),
            /// There are instances when extract_location() will encounter a value in the DWARF definition, rather than a memory location where the value can be read.
            /// In those cases it will set Variable.value, and set Variable.location to u64::MAX, which tells the Variable.extract_value() to NOT overwrite it.
            memory_location: 0,
            ..Default::default()
        }
    }

    /// Implementing set_value(), because the library passes errors into the value of the variable.
    /// This ensures debug front ends can see the errors, but doesn't fail because of a single variable not being able to decode correctly.
    pub fn set_value(&mut self, new_value: String) {
        if self.value.is_empty() {
            self.value = new_value;
        } else {
            // We append the new value to the old value, so that we don't loose any prior errors or warnings originating from the process of decoding the actual value.
            self.value = format!("{} : {}", self.value, new_value);
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self) -> String {
        self.value.clone()
    }

    /// Evaluate the variable's result if possible and set self.value, or else set self.value as the error String.
    pub fn extract_value(&mut self, core: &mut Core<'_>) {
        if self.kind == VariableKind::Pointer {
            self.inclusion = VariableInclusion::Include;
            self.value = self.type_name.clone();
            return;
        } else {
            // Since extract_value is called very late in the decoding process, we can defer setting of the VariableKind until this point.
            if self.name.starts_with("__") {
                self.kind = VariableKind::Indexed;
            } else {
                self.kind = VariableKind::Named;
            }
        }

        // Quick exit if we don't really need to do much more.
        // The value was set by get_location(), so just leave it as is.
        if self.memory_location == u64::MAX
        // The value was set elsewhere in this library - probably because of an error - so just leave it as is.
        || !self.value.is_empty()
        // Templates, Phantoms, etc.
        || self.memory_location.is_zero()
        {
            return;
        }
        // This is the primary logic for decoding a variable's value, once we know the type and memory_location.
        let string_value = match self.type_name.as_str() {
            "!" => "<Never returns>".to_string(),
            "()" => "()".to_string(),
            "bool" => bool::get_value(self, core)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "char" => char::get_value(self, core)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "&str" => {
                let string_value = String::get_value(self, core)
                    .map_or_else(|err| format!("ERROR: {:?}", err), |value| value);
                // We don't need these for debugging purposes ... unless we get the ERROR below.
                self.children = None;
                string_value
            }
            "i8" => i8::get_value(self, core)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "i16" => i16::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "i32" => i32::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "i64" => i64::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "i128" => i128::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "isize" => isize::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "u8" => u8::get_value(self, core)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u16" => u16::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "u32" => u32::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "u64" => u64::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "u128" => u128::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "usize" => usize::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "f32" => f32::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "f64" => f64::get_value(self, core).map_or_else(
                |err| format!("ERROR: {:?}", err),
                |value| value.separate_with_underscores(),
            ),
            "None" => "None".to_string(),
            oops => match &self.children {
                Some(_children) => {
                    if oops.is_empty() {
                        "ERROR: This is a bug! Attempted to evaluate an empty Type".to_string()
                    } else {
                        format!("{}", self)
                    }
                }
                None => {
                    format!(
                        "UNIMPLEMENTED: Evaluate type {} of ({} bytes) at location 0x{:08x}",
                        oops, self.byte_size, self.memory_location
                    )
                }
            },
        };
        self.value = string_value;
    }

    /// Instead of just pushing to Variable.children, do some intelligent selection/addition of new Variables.
    /// Primarily this is to force late-as-possible(before parent) call of `extract_value()` on child variables,
    /// and to determine which of the processed DWARF nodes are included in the final variable tree.
    pub fn add_child_variable(&mut self, child_variable: &mut Variable, core: &mut Core<'_>) {
        if !(child_variable.inclusion == VariableInclusion::Undetermined
            || child_variable.inclusion == VariableInclusion::Exclude)
        {
            // Just-in-Time creation of Vec to store the children.
            let children: &mut Vec<Variable> = match &mut self.children {
                Some(children) => children,
                None => {
                    self.children = Some(vec![]);
                    self.children.as_mut().unwrap()
                }
            };
            // Warning, child_variable's VariableInclusion might have changed after this line.
            // Ensure parent inclusion setting honours the child inclusion.
            child_variable.extract_value(core);
            self.inclusion = VariableInclusion::Include;
            if child_variable.inclusion == VariableInclusion::Include {
                // Check to see if this child already exists - We need to do this,
                // because cargo's `codegen-units` sometimes spread and/or repeat namespace children between them.
                if let Some(existing_child) = children.iter_mut().find(|current_child| {
                    current_child.name == child_variable.name
                        && current_child.type_name == child_variable.type_name
                }) {
                    // Just add the children (if there are any) from the new child to the existing child
                    if let Some(new_children) = child_variable.children.clone() {
                        for mut new_child in new_children {
                            existing_child.add_child_variable(&mut new_child, core);
                        }
                    }
                } else {
                    children.push(child_variable.clone());
                }
            }
        }
    }
}

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.value.is_empty() {
            // Only do this if we do not already have a value assigned.
            if let Some(children) = self.children.clone() {
                // Make sure we can safely unwrap() children.
                if self.type_name.starts_with('&') {
                    // Pointers
                    write!(f, "{}", children.first().unwrap())
                } else if self.type_name.starts_with('(') {
                    // Tuples
                    write!(f, "(")?;
                    for child in children {
                        write!(f, "{}, ", child)?;
                    }
                    write!(f, ")")
                } else if self.type_name.starts_with('[') {
                    // Arrays
                    write!(f, "[")?;
                    for child in children {
                        write!(f, "{}, ", child)?;
                    }
                    write!(f, "]")
                } else {
                    // Generic handling of other structured types.
                    // TODO: This is 'ok' for most, but could benefit from some custom formatting, e.g. Unions.
                    if self.kind == VariableKind::Named {
                        write!(f, "{}:{{", self.name)?;
                    } else {
                        write!(f, "{{")?;
                    }
                    for child in children {
                        write!(f, "{}, ", child)?;
                    }
                    write!(f, "}}")
                }
            } else {
                // Unknown.
                write!(f, "{}", self.type_name)
            }
        } else {
            // Use the supplied value.
            write!(f, "{}", self.value)
        }
    }
}
/// Traits and Impl's to read from memory and decode the Variable value based on Variable::typ and Variable::location.
/// The MS DAP protocol passes the value as a string, so these are here only to provide the memory read logic before returning it as a string.
trait Value {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError>
    where
        Self: Sized;
}

impl Value for bool {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mem_data = core.read_word_8(variable.memory_location as u32)?;
        let ret_value: bool = mem_data != 0;
        Ok(ret_value)
    }
}
impl Value for char {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mem_data = core.read_word_32(variable.memory_location as u32)?;
        // TODO: Use char::from_u32 once it stabilizes.
        let ret_value: char = mem_data.try_into()?;
        Ok(ret_value)
    }
}

impl Value for String {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let str_value: String;
        match variable.clone().children {
            Some(children) => {
                let string_length = match children
                    .clone()
                    .into_iter()
                    .find(|child_variable| child_variable.name == *"length")
                {
                    Some(length_value) => length_value.value.parse().unwrap_or(0) as usize,
                    None => 0_usize,
                };
                let string_location = match children
                    .into_iter()
                    .find(|child_variable| child_variable.name == *"data_ptr")
                {
                    Some(location_value) => {
                        if let Some(child_variables) = location_value.children {
                            if let Some(first_child) = child_variables.first() {
                                first_child.memory_location as u32
                            } else {
                                0_u32
                            }
                        } else {
                            0_u32
                        }
                    }
                    None => 0_u32,
                };
                if string_location.is_zero() {
                    str_value = "ERROR: Failed to determine &str memory location".to_string();
                } else {
                    let mut buff = vec![0u8; string_length];
                    core.read(string_location as u32, &mut buff)?;
                    str_value = core::str::from_utf8(&buff)?.to_owned();
                }
            }
            None => {
                str_value = "ERROR: Failed to evaluate &str value".to_string();
            }
        };
        Ok(str_value)
    }
}
impl Value for i8 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i8::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i16 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i16::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i32 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i64 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i128 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i128::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for isize {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        // TODO: how to get the MCU isize calculated for all platforms.
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value as isize)
    }
}

impl Value for u8 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u8::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u16 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u16::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u32 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u64 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u128 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u128::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for usize {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        // TODO: how to get the MCU usize calculated for all platforms.
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value as usize)
    }
}
impl Value for f32 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = f32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for f64 {
    fn get_value(variable: &Variable, core: &mut Core<'_>) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = f64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
