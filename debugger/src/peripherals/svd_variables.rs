use crate::DebuggerError;
use probe_rs::{
    debug::{Variable, VariableCache, VariableName},
    Core,
};
use std::{fmt::Debug, fs::File, io::Read, path::Path};
use svd_parser::{self as svd, ValidateLevel};
use svd_rs::{Access, Device, EnumeratedValues, FieldInfo, PeripheralInfo, RegisterInfo};

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
    pub(crate) fn new(svd_file: &Path, core: &mut Core) -> Result<Self, DebuggerError> {
        let svd_xml = &mut String::new();
        match File::open(svd_file) {
            Ok(mut svd_opened_file) => {
                let _ = svd_opened_file.read_to_string(svd_xml);
                match svd::parse(svd_xml) {
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
        peripheral_variable.type_name = peripheral
            .description
            .clone()
            .unwrap_or_else(|| "Device Peripheral".to_string());
        peripheral_variable.variable_node_type = probe_rs::debug::VariableNodeType::SvdPeripheral;
        peripheral_variable.memory_location = peripheral.base_address;
        peripheral_variable.set_value(probe_rs::debug::VariableValue::Valid(
            peripheral
                .description
                .clone()
                .unwrap_or_else(|| format!("{}", peripheral_variable.name)),
        ));
        peripheral_variable = svd_cache.cache_variable(
            Some(device_root_variable.variable_key),
            peripheral_variable,
            core,
        )?;
        for register in &resolve_registers(peripheral)? {
            let mut register_variable = Variable::new(None, None);
            register_variable.name = VariableName::Named(format!(
                "{}.{}",
                &peripheral_variable.name,
                register.name.clone()
            ));
            register_variable.type_name = register
                .description
                .clone()
                .unwrap_or_else(|| "Peripheral Register".to_string());
            register_variable.variable_node_type = probe_rs::debug::VariableNodeType::SvdRegister;
            register_variable.memory_location =
                peripheral.base_address + register.address_offset as u64;
            let mut register_has_restricted_read = false;
            if register.read_action.is_some()
                || (if let Some(register_access) = register.properties.access {
                    register_access == Access::ReadWriteOnce || register_access == Access::WriteOnly
                } else {
                    false
                })
            {
                register_variable.set_value(probe_rs::debug::VariableValue::Error(
                    "Register access doesn't allow reading, or will have side effects.".to_string(),
                ));
                register_has_restricted_read = true;
            }
            register_variable = svd_cache.cache_variable(
                Some(peripheral_variable.variable_key),
                register_variable,
                core,
            )?;
            for field in &resolve_fields(register)? {
                let mut field_variable = Variable::new(None, None);
                field_variable.name = VariableName::Named(format!(
                    "{}.{}",
                    &register_variable.name,
                    field.name.clone()
                ));
                field_variable.type_name = field
                    .description
                    .clone()
                    .unwrap_or_else(|| "Register Field".to_string());
                field_variable.variable_node_type = probe_rs::debug::VariableNodeType::SvdField;
                field_variable.memory_location = register_variable.memory_location;
                // For SVD fields, we overload the range_lower_bound and range_upper_bound as the bit range LSB and MSB.
                field_variable.range_lower_bound = field.bit_offset() as i64;
                field_variable.range_upper_bound = (field.bit_offset() + field.bit_width()) as i64;
                if register_has_restricted_read {
                    register_variable.set_value(probe_rs::debug::VariableValue::Error(
                        "Register access doesn't allow reading, or will have side effects."
                            .to_string(),
                    ));
                } else if field.read_action.is_some()
                    || (if let Some(field_access) = field.access {
                        field_access == Access::ReadWriteOnce || field_access == Access::WriteOnly
                    } else {
                        false
                    })
                {
                    field_variable.set_value(probe_rs::debug::VariableValue::Error(
                        "Field access doesn't allow reading, or will have side effects."
                            .to_string(),
                    ));
                    // If we can't read any of the bits, then don't read the register either.
                    register_variable.set_value(probe_rs::debug::VariableValue::Error(
                        "Some fields' access doesn't allow reading, or will have side effects."
                            .to_string(),
                    ));
                    register_has_restricted_read = true;
                    register_variable = svd_cache.cache_variable(
                        Some(peripheral_variable.variable_key),
                        register_variable,
                        core,
                    )?;
                }
                // TODO: Extend the Variable definition, so that we can resolve the EnumeratedValues for fields.
                svd_cache.cache_variable(
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
        // Deriving the properties starts from the peripheral level defaults.
        let mut register_properties = peripheral.default_register_properties;
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
                        .modified_write_values(template_register.modified_write_values);
                }
                if template_register.write_constraint.is_some() {
                    register_builder =
                        register_builder.write_constraint(template_register.write_constraint);
                }
                if template_register.read_action.is_some() {
                    register_builder = register_builder.read_action(template_register.read_action);
                }
                if template_register.fields.is_some() {
                    register_builder = register_builder.fields(template_register.fields.clone());
                }
                // We don't update the register_builder properties directly until the next step.
                if template_register.properties.size.is_some() {
                    register_properties =
                        register_properties.size(template_register.properties.size);
                }
                if template_register.properties.access.is_some() {
                    register_properties =
                        register_properties.access(template_register.properties.access);
                }
                if template_register.properties.protection.is_some() {
                    register_properties =
                        register_properties.protection(template_register.properties.protection);
                }
                if template_register.properties.reset_value.is_some() {
                    register_properties =
                        register_properties.reset_value(template_register.properties.reset_value);
                }
                if template_register.properties.reset_mask.is_some() {
                    register_properties =
                        register_properties.reset_mask(template_register.properties.reset_mask);
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD register: {:?}",
                    derived_from
                )));
            };
        }
        // Irrespective of derived_from values, set the values we need.
        let mut register_name = peripheral_register.name.clone();
        if let Some(prefix) = &peripheral.prepend_to_name {
            register_name = format!("{}{}", prefix, register_name);
        }
        if let Some(suffix) = &peripheral.append_to_name {
            register_name = format!("{}{}", register_name, suffix);
        }
        register_builder = register_builder.name(register_name);
        if peripheral_register.display_name.is_some() {
            register_builder =
                register_builder.display_name(peripheral_register.display_name.clone());
        }
        if peripheral_register.description.is_some() {
            register_builder =
                register_builder.description(peripheral_register.description.clone());
        }
        register_builder = register_builder.address_offset(peripheral_register.address_offset);
        register_builder = register_builder.properties(peripheral_register.properties);
        if peripheral_register.modified_write_values.is_some() {
            register_builder =
                register_builder.modified_write_values(peripheral_register.modified_write_values);
        }
        if peripheral_register.write_constraint.is_some() {
            register_builder =
                register_builder.write_constraint(peripheral_register.write_constraint);
        }
        if peripheral_register.read_action.is_some() {
            register_builder = register_builder.read_action(peripheral_register.read_action);
        }
        if peripheral_register.fields.is_some() {
            register_builder = register_builder.fields(peripheral_register.fields.clone());
        }
        // Complete the derive of the register properties.
        if peripheral_register.properties.size.is_some() {
            register_properties = register_properties.size(peripheral_register.properties.size);
        }
        if peripheral_register.properties.access.is_some() {
            register_properties = register_properties.access(peripheral_register.properties.access);
        }
        if peripheral_register.properties.protection.is_some() {
            register_properties =
                register_properties.protection(peripheral_register.properties.protection);
        }
        if peripheral_register.properties.reset_value.is_some() {
            register_properties =
                register_properties.reset_value(peripheral_register.properties.reset_value);
        }
        if peripheral_register.properties.reset_mask.is_some() {
            register_properties =
                register_properties.reset_mask(peripheral_register.properties.reset_mask);
        }
        register_builder = register_builder.properties(register_properties);
        // Not that the register_builder has been updated, we can build it.
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
                    field_builder = field_builder.access(template_field.access);
                }
                if template_field.modified_write_values.is_some() {
                    field_builder =
                        field_builder.modified_write_values(template_field.modified_write_values);
                }
                if template_field.write_constraint.is_some() {
                    field_builder = field_builder.write_constraint(template_field.write_constraint);
                }
                if template_field.read_action.is_some() {
                    field_builder = field_builder.read_action(template_field.read_action);
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
        field_builder = field_builder.bit_range(register_field.bit_range);
        field_builder = field_builder.access(register_field.access);
        if register_field.modified_write_values.is_some() {
            field_builder =
                field_builder.modified_write_values(register_field.modified_write_values);
        }
        if register_field.write_constraint.is_some() {
            field_builder = field_builder.write_constraint(register_field.write_constraint);
        }
        if register_field.read_action.is_some() {
            field_builder = field_builder.read_action(register_field.read_action);
        }
        field_builder = field_builder.enumerated_values(register_field.enumerated_values.clone());
        let resolved_field = field_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        resolved_fields.push(resolved_field);
    }
    Ok(resolved_fields)
}

// TODO: Implement using these enumerated values for SVD fields.
#[allow(dead_code)]
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
                    enum_values_builder = enum_values_builder.usage(template_enum_values.usage);
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
            enum_values_builder = enum_values_builder.usage(field_enum_values.usage);
        }
        enum_values_builder = enum_values_builder.values(field_enum_values.values.clone());
        let resolved_enum_values = enum_values_builder
            .build(ValidateLevel::Weak)
            .map_err(|error| DebuggerError::Other(anyhow::anyhow!("{:?}", error)))?;
        enumerated_values.push(resolved_enum_values);
    }
    Ok(enumerated_values)
}
