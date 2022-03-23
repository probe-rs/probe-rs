use crate::DebuggerError;
use probe_rs::{
    debug::{DebugError, Variable, VariableCache, VariableName},
    Core,
};
use std::{any, fmt::Debug, fs::File, io::Read, path::PathBuf};
use svd_parser::{self as svd, ValidateLevel};
use svd_rs::{peripheral, Device, EnumeratedValues, FieldInfo, PeripheralInfo, RegisterInfo};

/// The SVD file contents and related data
#[derive(Debug)]
pub(crate) struct SvdCache {
    /// The SVD contents and structure will be stored as variables, down to the Field level.
    /// Unlike other VariableCache instances, it will only be built once per DebugSession.
    /// After that, only the SVD fields values change values, and the data for these will be re-read everytime they are queried by the debugger.
    pub(crate) svd_variable_cache: VariableCache,
}

impl SvdCache {
    /// Create the SVD cache for a specific core. This function loads the file, parses it, and then builds the VariableCache.
    pub(crate) fn new(svd_file: &PathBuf, core: &mut Core) -> Result<Self, DebuggerError> {
        let svd_xml = &mut String::new();
        match File::open(svd_file.as_path()) {
            Ok(mut svd_opened_file) => {
                svd_opened_file.read_to_string(svd_xml);
                match svd::parse(&svd_xml) {
                    Ok(peripheral_device) => Ok(SvdCache {
                        svd_variable_cache: variable_cache_from_svd(peripheral_device, core)?,
                    }),
                    Err(error) => Err(DebuggerError::Other(anyhow::anyhow!(
                        "Unable to parse CMSIS-SVD file: {:?}. {:?}",
                        svd_file,
                        error,
                    ))),
                }
            }
            Err(error) => Err(DebuggerError::Other(anyhow::anyhow!("{}", error))),
        }
    }
}

/// Create a [`probe_rs::debug::VariableCache`] from a Device that was parsed from a CMSIS-SVD file.
pub(crate) fn variable_cache_from_svd(
    peripheral_device: Device,
    core: &mut Core,
) -> Result<probe_rs::debug::VariableCache, DebuggerError> {
    let mut svd_cache = probe_rs::debug::VariableCache::new();
    let mut device_root_variable = Variable::new(None, None);
    device_root_variable.variable_node_type = probe_rs::debug::VariableNodeType::DoNotRecurse;
    device_root_variable.name = VariableName::PeripheralScopeRoot;
    device_root_variable = svd_cache.cache_variable(None, device_root_variable, core)?;
    for peripheral in &resolve_peripherals(&peripheral_device)? {
        // TODO: Create a parent structure for peripheral groups with more than one member.
        let mut peripheral_variable = Variable::new(None, None);
        peripheral_variable.name = VariableName::Named(peripheral.name.clone());
        peripheral_variable.type_name = "SvdPeripheral".to_string();
        peripheral_variable.variable_node_type = probe_rs::debug::VariableNodeType::DirectLookup;
        peripheral_variable.memory_location = peripheral.base_address;
        peripheral_variable = svd_cache.cache_variable(
            Some(device_root_variable.variable_key),
            peripheral_variable,
            core,
        )?;
        for register in &resolve_registers(peripheral)? {
            let mut register_variable = Variable::new(None, None);
            register_variable.name = VariableName::Named(register.name.clone());
            register_variable.type_name = "SvdRegister".to_string();
            register_variable.variable_node_type = probe_rs::debug::VariableNodeType::DirectLookup;
            register_variable.memory_location =
                peripheral.base_address + register.address_offset as u64;
            register_variable = svd_cache.cache_variable(
                Some(peripheral_variable.variable_key),
                register_variable,
                core,
            )?;
            for field in &resolve_fields(register)? {
                let mut field_variable = Variable::new(None, None);
                field_variable.name = VariableName::Named(field.name.clone());
                field_variable.type_name = "SvdField".to_string();
                field_variable.variable_node_type = probe_rs::debug::VariableNodeType::DirectLookup;
                field_variable.memory_location = register_variable.memory_location;
                // For SVD fields, we overload the range_lower_bound and range_upper_bound as the bit range LSB and MSB.
                field_variable.range_lower_bound = field.bit_offset() as i64;
                field_variable.range_upper_bound = (field.bit_offset() + field.bit_width()) as i64;
                field_variable = svd_cache.cache_variable(
                    Some(register_variable.variable_key),
                    field_variable,
                    core,
                )?;
            }
        }
    }

    Ok(svd_cache)
}

/// Resolve all the peripherals through their (optional) `derived_from` peripheral.
pub(crate) fn resolve_peripherals(
    peripheral_device: &Device,
) -> Result<Vec<PeripheralInfo>, DebuggerError> {
    let mut resolved_peripherals = vec![];
    for device_peripheral in &peripheral_device.peripherals {
        // TODO: Need to code for the impact of MaybeArray results.
        let mut peripheral_builder = PeripheralInfo::builder();
        if let Some(derived_from) = &device_peripheral.derived_from {
            if let Some(template_peripheral) = peripheral_device.get_peripheral(derived_from) {
                if template_peripheral.group_name.is_some() {
                    peripheral_builder =
                        peripheral_builder.group_name(template_peripheral.group_name.clone());
                }
                if template_peripheral.display_name.is_some() {
                    peripheral_builder =
                        peripheral_builder.display_name(template_peripheral.display_name.clone());
                }
                if template_peripheral.description.is_some() {
                    peripheral_builder =
                        peripheral_builder.description(template_peripheral.description.clone());
                }
                if template_peripheral.prepend_to_name.is_some() {
                    peripheral_builder = peripheral_builder
                        .prepend_to_name(template_peripheral.prepend_to_name.clone());
                }
                if template_peripheral.append_to_name.is_some() {
                    peripheral_builder = peripheral_builder
                        .append_to_name(template_peripheral.append_to_name.clone());
                }
                peripheral_builder =
                    peripheral_builder.base_address(template_peripheral.base_address);
                peripheral_builder = peripheral_builder
                    .default_register_properties(template_peripheral.default_register_properties);
                if template_peripheral.address_block.is_some() {
                    peripheral_builder =
                        peripheral_builder.address_block(template_peripheral.address_block.clone());
                }
                peripheral_builder =
                    peripheral_builder.interrupt(Some(template_peripheral.interrupt.clone()));
                if template_peripheral.registers.is_some() {
                    peripheral_builder =
                        peripheral_builder.registers(template_peripheral.registers.clone());
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD peripheral: {:?}",
                    derived_from
                )));
            };
        }
        // Irrespective of derived_from values, set the values we need.
        peripheral_builder = peripheral_builder.name(device_peripheral.name.clone());
        peripheral_builder = peripheral_builder.description(device_peripheral.description.clone());
        if device_peripheral.description.is_some() {
            peripheral_builder =
                peripheral_builder.description(device_peripheral.description.clone());
        }
        if device_peripheral.display_name.is_some() {
            peripheral_builder =
                peripheral_builder.display_name(device_peripheral.display_name.clone());
        }
        if device_peripheral.group_name.is_some() {
            peripheral_builder =
                peripheral_builder.group_name(device_peripheral.group_name.clone());
        }
        if device_peripheral.prepend_to_name.is_some() {
            peripheral_builder =
                peripheral_builder.prepend_to_name(device_peripheral.prepend_to_name.clone());
        }
        if device_peripheral.append_to_name.is_some() {
            peripheral_builder =
                peripheral_builder.append_to_name(device_peripheral.append_to_name.clone());
        }
        peripheral_builder = peripheral_builder.base_address(device_peripheral.base_address);
        peripheral_builder = peripheral_builder
            .default_register_properties(device_peripheral.default_register_properties);
        if device_peripheral.address_block.is_some() {
            peripheral_builder =
                peripheral_builder.address_block(device_peripheral.address_block.clone());
        }
        peripheral_builder =
            peripheral_builder.interrupt(Some(device_peripheral.interrupt.clone()));
        if device_peripheral.registers.is_some() {
            peripheral_builder = peripheral_builder.registers(device_peripheral.registers.clone());
        }
        let resolved_peripheral = peripheral_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        resolved_peripherals.push(resolved_peripheral);
    }
    Ok(resolved_peripherals)
}

/// Resolve all the registers of a peripheral through their (optional) `derived_from` register.
pub(crate) fn resolve_registers(
    peripheral: &PeripheralInfo,
) -> Result<Vec<RegisterInfo>, DebuggerError> {
    // TODO: Need to code for the impact of register clusters.
    let mut resolved_registers = vec![];
    for peripheral_register in peripheral.registers() {
        // TODO: Need to code for the impact of MaybeArray results.
        let mut register_builder = RegisterInfo::builder();
        if let Some(derived_from) = &peripheral_register.derived_from {
            if let Some(template_register) = peripheral.get_register(derived_from) {
                if template_register.display_name.is_some() {
                    register_builder =
                        register_builder.display_name(template_register.display_name.clone());
                }
                if template_register.description.is_some() {
                    register_builder =
                        register_builder.description(template_register.description.clone());
                }
                if template_register.modified_write_values.is_some() {
                    register_builder = register_builder
                        .modified_write_values(template_register.modified_write_values.clone());
                }
                if template_register.write_constraint.is_some() {
                    register_builder = register_builder
                        .write_constraint(template_register.write_constraint.clone());
                }
                if template_register.read_action.is_some() {
                    register_builder =
                        register_builder.read_action(template_register.read_action.clone());
                }
                if template_register.fields.is_some() {
                    register_builder = register_builder.fields(template_register.fields.clone());
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD register: {:?}",
                    derived_from
                )));
            };
        }
        // Irrespective of derived_from values, set the values we need.
        register_builder = register_builder.name(peripheral_register.name.clone());
        if peripheral_register.display_name.is_some() {
            register_builder =
                register_builder.display_name(peripheral_register.display_name.clone());
        }
        if peripheral_register.description.is_some() {
            register_builder =
                register_builder.description(peripheral_register.description.clone());
        }
        register_builder =
            register_builder.address_offset(peripheral_register.address_offset.clone());
        register_builder = register_builder.properties(peripheral_register.properties.clone());
        if peripheral_register.modified_write_values.is_some() {
            register_builder = register_builder
                .modified_write_values(peripheral_register.modified_write_values.clone());
        }
        if peripheral_register.write_constraint.is_some() {
            register_builder =
                register_builder.write_constraint(peripheral_register.write_constraint.clone());
        }
        if peripheral_register.read_action.is_some() {
            register_builder =
                register_builder.read_action(peripheral_register.read_action.clone());
        }
        if peripheral_register.fields.is_some() {
            register_builder = register_builder.fields(peripheral_register.fields.clone());
        }
        let resolved_register = register_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        resolved_registers.push(resolved_register);
    }
    Ok(resolved_registers)
}

/// Resolve all the fields of a register through their (optional) `derived_from` field.
pub(crate) fn resolve_fields(register: &RegisterInfo) -> Result<Vec<FieldInfo>, DebuggerError> {
    // TODO: Need to code for the impact of field clusters.
    let mut resolved_fields = vec![];
    for register_field in register.fields() {
        // TODO: Need to code for the impact of MaybeArray results.
        let mut field_builder = FieldInfo::builder();
        if let Some(derived_from) = &register_field.derived_from {
            if let Some(template_field) = register.get_field(derived_from) {
                if template_field.description.is_some() {
                    field_builder = field_builder.description(template_field.description.clone());
                }
                if template_field.access.is_some() {
                    field_builder = field_builder.access(template_field.access.clone());
                }
                if template_field.modified_write_values.is_some() {
                    field_builder = field_builder
                        .modified_write_values(template_field.modified_write_values.clone());
                }
                if template_field.write_constraint.is_some() {
                    field_builder =
                        field_builder.write_constraint(template_field.write_constraint.clone());
                }
                if template_field.read_action.is_some() {
                    field_builder = field_builder.read_action(template_field.read_action.clone());
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD field: {:?}",
                    derived_from
                )));
            };
        }
        // Irrespective of derived_from values, set the values we need.
        field_builder = field_builder.name(register_field.name.clone());
        if register_field.description.is_some() {
            field_builder = field_builder.description(register_field.description.clone());
        }
        field_builder = field_builder.bit_range(register_field.bit_range.clone());
        field_builder = field_builder.access(register_field.access.clone());
        if register_field.modified_write_values.is_some() {
            field_builder =
                field_builder.modified_write_values(register_field.modified_write_values.clone());
        }
        if register_field.write_constraint.is_some() {
            field_builder = field_builder.write_constraint(register_field.write_constraint.clone());
        }
        if register_field.read_action.is_some() {
            field_builder = field_builder.read_action(register_field.read_action.clone());
        }
        field_builder = field_builder.enumerated_values(register_field.enumerated_values.clone());
        let resolved_field = field_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        resolved_fields.push(resolved_field);
    }
    Ok(resolved_fields)
}

/// Resolve all the enumerated values of a field through their (optional) `derived_from` values.
pub(crate) fn enumerated_values(field: FieldInfo) -> Result<Vec<EnumeratedValues>, DebuggerError> {
    // TODO: Need to code for the impact of enumerated value clusters.
    let mut enumerated_values = vec![];
    for field_enum_values in &field.enumerated_values {
        // TODO: Need to code for the impact of MaybeArray results.
        let mut enum_values_builder = EnumeratedValues::builder();
        if let Some(derived_from) = &field_enum_values.derived_from {
            if let Some(template_enum_values) =
                field.enumerated_values.iter().find(|derived_from_values| {
                    derived_from_values.name == Some(derived_from.to_owned())
                })
            {
                if template_enum_values.name.is_some() {
                    enum_values_builder =
                        enum_values_builder.name(template_enum_values.name.clone());
                }
                if template_enum_values.usage.is_some() {
                    enum_values_builder =
                        enum_values_builder.usage(template_enum_values.usage.clone());
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD field: {:?}",
                    derived_from
                )));
            };
        }
        // Irrespective of derived_from values, set the values we need.
        if field_enum_values.name.is_some() {
            enum_values_builder = enum_values_builder.name(field_enum_values.name.clone());
        }
        if field_enum_values.usage.is_some() {
            enum_values_builder = enum_values_builder.usage(field_enum_values.usage.clone());
        }
        enum_values_builder = enum_values_builder.values(field_enum_values.values.clone());
        let resolved_enum_values = enum_values_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        enumerated_values.push(resolved_enum_values);
    }
    Ok(enumerated_values)
}
