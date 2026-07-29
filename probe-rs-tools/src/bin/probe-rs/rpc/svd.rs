//! Server-side CMSIS-SVD parser and peripheral variable cache.
//!
//! The RPC server owns the live `probe_rs::Core`, so peripheral register
//! reads happen here. The DAP client never builds an SVD cache; it uploads the
//! SVD file (via the temp-file endpoints) and calls `load_svd`, which parses
//! the file and stores the resulting [`SvdVariableCache`] in the per-core
//! [`crate::rpc::debug_state::CoreDebugState`].

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use probe_rs::MemoryInterface;
use probe_rs_debug::{DebugError, ObjectRef, get_object_reference};

/// Cache of SVD-derived variables for one core, structured down to the field
/// level. Built once per debug session from the CMSIS-SVD file; only the
/// register/field *values* are re-read from the target on each query.
#[derive(Debug, Clone, PartialEq)]
pub struct SvdVariableCache {
    root_variable_key: ObjectRef,
    variable_hash_map: BTreeMap<ObjectRef, Variable>,
}

impl SvdVariableCache {
    /// Create a new, empty cache with a root variable.
    pub(crate) fn new_svd_cache() -> Self {
        let root_variable_key = get_object_reference();
        let root_variable = Variable {
            variable_key: root_variable_key,
            parent_key: ObjectRef::Invalid,
            name: "Peripheral variable".to_string(),
            variable_kind: SvdVariable::Root,
        };

        SvdVariableCache {
            root_variable_key,
            variable_hash_map: BTreeMap::from([(root_variable_key, root_variable)]),
        }
    }

    /// All children of `parent_key` (top-level variables when `parent_key` is
    /// [`ObjectRef::Invalid`]).
    pub fn get_children(&self, parent_key: ObjectRef) -> Vec<&Variable> {
        self.variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_key)
            .collect::<Vec<_>>()
    }

    /// Look up a variable by its key.
    pub fn get_variable_by_key(&self, variable_key: ObjectRef) -> Option<&Variable> {
        self.variable_hash_map.get(&variable_key)
    }

    /// The root variable's key (the `variables_reference` for the Peripherals
    /// scope).
    pub fn root_variable_key(&self) -> ObjectRef {
        self.root_variable_key
    }

    fn add_variable(
        &mut self,
        parent_key: ObjectRef,
        name: String,
        variable: SvdVariable,
    ) -> Result<ObjectRef, DebugError> {
        let variable_key = get_object_reference();
        let cache_variable = Variable {
            variable_key,
            parent_key,
            name,
            variable_kind: variable,
        };

        // Validate that the parent_key exists ...
        if !self.variable_hash_map.contains_key(&parent_key) {
            return Err(DebugError::Other(format!(
                "SvdVariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug",
                cache_variable.name, parent_key
            )));
        }

        if let Some(old_variable) = self
            .variable_hash_map
            .insert(cache_variable.variable_key, cache_variable.clone())
        {
            return Err(DebugError::Other(format!(
                "Attempt to insert a new `SvdVariable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.",
                cache_variable.name, old_variable.variable_key
            )));
        }

        Ok(cache_variable.variable_key)
    }

    /// Retrieve a variable by name (logs a warning if more than one matches).
    fn get_variable_by_name_and_parent(
        &self,
        variable_name: &str,
        parent_key: ObjectRef,
    ) -> Option<&Variable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .collect::<Vec<&Variable>>();

        match &child_variables[..] {
            [] => None,
            [variable] => Some(*variable),
            [.., last] => {
                tracing::error!(
                    "Found {} variables with parent_key={:?} and name={}. Please report this as a bug.",
                    child_variables.len(),
                    parent_key,
                    variable_name
                );
                Some(last)
            }
        }
    }
}

/// A single SVD variable (root / peripheral group / peripheral / register / field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    variable_key: ObjectRef,
    parent_key: ObjectRef,
    name: String,
    pub variable_kind: SvdVariable,
}

impl Variable {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn variable_key(&self) -> ObjectRef {
        self.variable_key
    }

    /// Memory reference, compatible with DAP.
    pub fn memory_reference(&self) -> Option<String> {
        match self.variable_kind {
            SvdVariable::SvdRegister {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{address:#010X}")),
            SvdVariable::SvdField {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{address:#010X}")),
            _ => None,
        }
    }

    pub fn type_name(&self) -> Option<String> {
        self.variable_kind.type_name()
    }

    /// Read the variable's value from the target via `MemoryInterface`.
    pub fn get_value(&self, memory: &mut dyn MemoryInterface) -> String {
        self.variable_kind.get_value(memory)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvdVariable {
    Root,
    SvdRegister {
        address: u64,
        restricted_read: bool,
        description: Option<String>,
        size: u32,
    },
    SvdField {
        address: u64,
        restricted_read: bool,
        bit_range_lower_bound: u32,
        bit_range_upper_bound: u32,
        description: Option<String>,
    },
    SvdPeripheral {
        base_address: u64,
        description: Option<String>,
    },
    SvdPeripheralGroup {
        description: Option<String>,
    },
}

impl SvdVariable {
    fn get_value(&self, memory: &mut dyn MemoryInterface) -> String {
        match &self {
            SvdVariable::Root => "".to_string(),
            SvdVariable::SvdPeripheral { description, .. }
            | SvdVariable::SvdPeripheralGroup { description } => {
                description.as_ref().cloned().unwrap_or_default()
            }
            SvdVariable::SvdRegister {
                address,
                restricted_read,
                size,
                ..
            } => {
                if *restricted_read {
                    format!("Register cannot be read without side effects @ {address:#010X}")
                } else {
                    let size_bytes = (*size / 8).max(1);
                    let bits_alignment_offset = (*address % size_bytes as u64) as u32 * 8;
                    let value = match *size {
                        0..=8 => memory
                            .read_word_8(*address)
                            .map(|val| format!("{val:#04X}")),
                        9..=16 => {
                            let aligned_address = *address & !0b1;
                            memory
                                .read_word_16(aligned_address)
                                .map(|val| format!("{:#06X}", val >> bits_alignment_offset))
                        }
                        _ => {
                            let aligned_address = *address & !0b11;
                            memory
                                .read_word_32(aligned_address)
                                .map(|val| format!("{:#010X}", val >> bits_alignment_offset))
                        }
                    };

                    match value {
                        Ok(value) => value,
                        Err(error) => format!(
                            "Unable to read peripheral register value @ {address:#010X} : {error:?}"
                        ),
                    }
                }
            }
            SvdVariable::SvdField {
                address,
                restricted_read,
                bit_range_lower_bound,
                bit_range_upper_bound,
                ..
            } => {
                if *restricted_read {
                    format!("Field cannot be read without side effects @ {address:#010X}")
                } else {
                    let value = match *bit_range_upper_bound {
                        0..8 => memory.read_word_8(*address).map(u32::from),
                        8..16 => memory.read_word_16(*address & !0b1).map(u32::from),
                        _ => memory.read_word_32(*address & !0b11),
                    };

                    match value {
                        Ok(register_u32_value) => {
                            let mut bit_value: u32 = register_u32_value;
                            bit_value <<= 32 - bit_range_upper_bound;
                            bit_value >>= 32 - (bit_range_upper_bound - bit_range_lower_bound);
                            format!(
                                "{:0width$b} @ {:#010X}:{}..{}",
                                bit_value,
                                address,
                                bit_range_lower_bound,
                                bit_range_upper_bound,
                                width = (*bit_range_upper_bound - *bit_range_lower_bound) as usize
                            )
                        }
                        Err(error) => format!(
                            "Unable to read peripheral register field value @ {address:#010X} : {error:?}"
                        ),
                    }
                }
            }
        }
    }

    fn type_name(&self) -> Option<String> {
        match &self {
            SvdVariable::SvdRegister { description, .. }
            | SvdVariable::SvdField { description, .. } => description.clone(),
            SvdVariable::SvdPeripheral { .. } => Some("Peripheral".to_string()),
            SvdVariable::SvdPeripheralGroup { .. } => Some("Peripheral Group".to_string()),
            SvdVariable::Root => None,
        }
    }
}

/// Read and parse an SVD file at `path`, then build the variable cache.
#[tracing::instrument(skip_all)]
pub fn parse_svd_file(path: &Path) -> Result<SvdVariableCache, DebugError> {
    let mut svd_xml = String::new();
    std::fs::File::open(path)?.read_to_string(&mut svd_xml)?;

    let device = svd_parser::parse_with_config(
        &svd_xml,
        // `expand_properties` pushes each peripheral's / the device's
        // `defaultRegisterProperties` (including `access`) down onto the individual
        // registers, so a register whose `access` is still `None` afterwards is one
        // that is genuinely unspecified anywhere in the SVD tree.
        &svd_parser::Config::default()
            .expand(true)
            .expand_properties(true)
            .ignore_enums(true),
    )
    .map_err(|error| {
        DebugError::Other(format!(
            "Unable to parse CMSIS-SVD file: {path:?}. {error:?}"
        ))
    })?;

    build_svd_cache(&device)
}

/// Build an [`SvdVariableCache`] from a parsed SVD `Device`.
#[tracing::instrument(skip_all)]
fn build_svd_cache(
    peripheral_device: &svd_parser::svd::Device,
) -> Result<SvdVariableCache, DebugError> {
    let mut svd_cache = SvdVariableCache::new_svd_cache();
    let device_root_variable_key = svd_cache.root_variable_key();

    let device_default_access = peripheral_device.default_register_properties.access;

    for peripheral in &peripheral_device.peripherals {
        let current_peripheral_group_name = peripheral.group_name.as_ref();

        let peripheral_parent_key;

        // Adding the Peripheral Group Name as an additional level in the structure helps to keep the 'variable tree' more compact,
        // but more importantly, it helps to avoid having duplicate variable names that conflict with hal crates.
        if let Some(peripheral_group_name) = &peripheral.group_name {
            // Before we create a new group variable, check if we have one by that name already.
            peripheral_parent_key = match svd_cache
                .get_variable_by_name_and_parent(peripheral_group_name, device_root_variable_key)
            {
                Some(existing_peripheral_group_variable) => {
                    existing_peripheral_group_variable.variable_key()
                }
                None => svd_cache.add_variable(
                    device_root_variable_key,
                    peripheral_group_name.clone(),
                    SvdVariable::SvdPeripheralGroup {
                        description: peripheral.description.clone(),
                    },
                )?,
            };
        } else {
            peripheral_parent_key = device_root_variable_key;
        }

        let peripheral_name = if let Some(peripheral_group) = current_peripheral_group_name {
            format!("{}.{}", peripheral_group, peripheral.name)
        } else {
            peripheral.name.clone()
        };

        let peripheral_key = svd_cache.add_variable(
            peripheral_parent_key,
            peripheral_name.clone(),
            SvdVariable::SvdPeripheral {
                base_address: peripheral.base_address,
                description: peripheral.description.clone(),
            },
        )?;

        for register in peripheral.all_registers() {
            let register_address = peripheral.base_address + register.address_offset as u64;

            // After property expansion above, a `None` access means no `<access>` was
            // declared anywhere in the tree (register, peripheral or device). Treat such
            // registers as readable: many recent SVDs (e.g. STM32H5) omit `<access>`
            // entirely, and defaulting to restricted made every register unviewable in
            // the debugger. Registers with a `read_action` are still restricted, so
            // side-effecting reads remain protected.
            let mut register_has_restricted_read = register.read_action.is_some()
                || register
                    .properties
                    .access
                    .map(|a| !a.can_read())
                    .or_else(|| device_default_access.map(|a| !a.can_read()))
                    .unwrap_or(false);

            let register_name = format!("{peripheral_name}.{}", register.name);

            let mut field_variables = Vec::new();

            for field in register.fields() {
                let field_has_restricted_read = register_has_restricted_read
                    || field.read_action.is_some()
                    || field
                        .access
                        .map(|a| !a.can_read())
                        .or_else(|| device_default_access.map(|a| !a.can_read()))
                        .unwrap_or(register_has_restricted_read);

                let field_variable = (
                    format!("{}.{}", register_name, field.name),
                    SvdVariable::SvdField {
                        address: register_address,
                        restricted_read: field_has_restricted_read,
                        bit_range_lower_bound: field.bit_offset(),
                        bit_range_upper_bound: (field.bit_offset() + field.bit_width()),
                        description: field.description.clone(),
                    },
                );

                // If any of the fields in the register have restricted read, then the register has restricted read.
                register_has_restricted_read |= field_has_restricted_read;

                field_variables.push(field_variable);
            }

            let register_variable_key = svd_cache.add_variable(
                peripheral_key,
                format!("{peripheral_name}.{}", register.name),
                SvdVariable::SvdRegister {
                    address: register_address,
                    restricted_read: register_has_restricted_read,
                    description: register.description.clone(),
                    size: register.properties.size.unwrap_or(32),
                },
            )?;

            for (variable_name, variable) in field_variables {
                // TODO: Extend the Variable definition, so that we can resolve the EnumeratedValues for fields.
                svd_cache.add_variable(register_variable_key, variable_name, variable)?;
            }
        }
    }

    Ok(svd_cache)
}
