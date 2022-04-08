use crate::{
    debug_adapter::{dap_adapter::DebugAdapter, protocol::ProtocolAdapter},
    DebuggerError,
};
use probe_rs::{
    debug::{
        Variable, VariableCache, VariableLocation, VariableName, VariableNodeType, VariableType,
    },
    Core,
};
use std::{fmt::Debug, fs::File, io::Read, path::Path};
use svd_parser::{self as svd};
use svd_rs::{Access, DeriveFrom, Device, FieldInfo, PeripheralInfo, RegisterInfo};

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
    pub(crate) fn new<P: ProtocolAdapter>(
        svd_file: &Path,
        core: &mut Core,
        debug_adapter: &mut DebugAdapter<P>,
        dap_request_id: i64,
    ) -> Result<Self, DebuggerError> {
        let svd_xml = &mut String::new();
        match File::open(svd_file) {
            Ok(mut svd_opened_file) => {
                let progress_id = debug_adapter.start_progress(
                    format!("Loading SVD file : {:?}", &svd_file).as_str(),
                    Some(dap_request_id),
                )?;
                let _ = svd_opened_file.read_to_string(svd_xml);
                let svd_cache = match svd::parse(svd_xml) {
                    Ok(peripheral_device) => {
                        debug_adapter
                            .update_progress(
                                None,
                                Some(format!("Done loading SVD file :{:?}", &svd_file)),
                                progress_id,
                            )
                            .ok();

                        Ok(SvdCache {
                            svd_variable_cache: variable_cache_from_svd(
                                peripheral_device,
                                core,
                                debug_adapter,
                                progress_id,
                            )?,
                        })
                    }
                    Err(error) => Err(DebuggerError::Other(anyhow::anyhow!(
                        "Unable to parse CMSIS-SVD file: {:?}. {:?}",
                        svd_file,
                        error,
                    ))),
                };
                let _ = debug_adapter.end_progress(progress_id)?;
                svd_cache
            }
            Err(error) => Err(DebuggerError::Other(anyhow::anyhow!("{}", error))),
        }
    }
}

/// Create a [`probe_rs::debug::VariableCache`] from a Device that was parsed from a CMSIS-SVD file.
pub(crate) fn variable_cache_from_svd<P: ProtocolAdapter>(
    peripheral_device: Device,
    core: &mut Core,
    debug_adapter: &mut DebugAdapter<P>,
    progress_id: i64,
) -> Result<probe_rs::debug::VariableCache, DebuggerError> {
    let mut svd_cache = probe_rs::debug::VariableCache::new();
    let mut device_root_variable = Variable::new(None, None);
    device_root_variable.variable_node_type = VariableNodeType::DoNotRecurse;
    device_root_variable.name = VariableName::PeripheralScopeRoot;
    device_root_variable = svd_cache.cache_variable(None, device_root_variable, core)?;
    // Adding the Peripheral Group Name as an additional level in the structure helps to keep the 'variable tree' more compact, but more importantly, it helps to avoid having duplicate variable names that conflict with hal crates.
    let mut peripheral_group_variable = Variable::new(None, None);
    peripheral_group_variable.name = VariableName::Named(peripheral_device.name.clone());
    let mut peripheral_parent_key = device_root_variable.variable_key;
    for peripheral in &resolve_peripherals(&peripheral_device)? {
        if let (Some(peripheral_group_name), VariableName::Named(variable_group_name)) =
            (&peripheral.group_name, &peripheral_group_variable.name)
        {
            if variable_group_name != peripheral_group_name {
                peripheral_group_variable = Variable::new(None, None);
                peripheral_group_variable.name = VariableName::Named(peripheral_group_name.clone());
                peripheral_group_variable.type_name =
                    VariableType::Other("Peripheral Group".to_string());
                peripheral_group_variable.variable_node_type = VariableNodeType::SvdPeripheral;
                peripheral_group_variable.set_value(probe_rs::debug::VariableValue::Valid(
                    peripheral
                        .description
                        .clone()
                        .unwrap_or_else(|| peripheral.name.clone()),
                ));
                peripheral_group_variable = svd_cache.cache_variable(
                    Some(device_root_variable.variable_key),
                    peripheral_group_variable,
                    core,
                )?;
                peripheral_parent_key = peripheral_group_variable.variable_key;
                debug_adapter
                    .update_progress(
                        None,
                        Some(format!(
                            "SVD loading peripheral group:{}",
                            &peripheral_group_name
                        )),
                        progress_id,
                    )
                    .ok();
            }
        }

        let mut peripheral_variable = Variable::new(None, None);
        peripheral_variable.name = VariableName::Named(format!(
            "{}.{}",
            peripheral_group_variable.name.clone(),
            peripheral.name.clone()
        ));
        peripheral_variable.type_name = VariableType::Other("Peripheral".to_string());
        peripheral_variable.variable_node_type = VariableNodeType::SvdPeripheral;
        peripheral_variable.memory_location =
            VariableLocation::Address(peripheral.base_address as u32);
        peripheral_variable.set_value(probe_rs::debug::VariableValue::Valid(
            peripheral
                .description
                .clone()
                .unwrap_or_else(|| format!("{}", peripheral_variable.name)),
        ));
        peripheral_variable =
            svd_cache.cache_variable(Some(peripheral_parent_key), peripheral_variable, core)?;
        for register in &resolve_registers(peripheral)? {
            let mut register_variable = Variable::new(None, None);
            register_variable.name = VariableName::Named(format!(
                "{}.{}",
                &peripheral_variable.name,
                register.name.clone()
            ));
            register_variable.type_name = VariableType::Other(
                register
                    .description
                    .clone()
                    .unwrap_or_else(|| "Peripheral Register".to_string()),
            );
            register_variable.variable_node_type = VariableNodeType::SvdRegister;
            register_variable.memory_location =
                VariableLocation::Address(peripheral.base_address as u32 + register.address_offset);
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
                field_variable.type_name = VariableType::Other(
                    field
                        .description
                        .clone()
                        .unwrap_or_else(|| "Register Field".to_string()),
                );
                field_variable.variable_node_type = VariableNodeType::SvdField;
                field_variable.memory_location = register_variable.memory_location.clone();
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
        if let Some(derived_from) = &device_peripheral.derived_from {
            if let Some(derived_result) = peripheral_device.get_peripheral(derived_from) {
                match &device_peripheral.derive_from(derived_result) {
                    svd_rs::MaybeArray::Single(derived_peripheral) => {
                        resolved_peripherals.push(derived_peripheral.clone());
                    }
                    svd_rs::MaybeArray::Array(peripheral_array, _) => {
                        log::warn!("Unsupported Array in SVD for Peripheral:{}. Only the first instance will be visible.", peripheral_array.name);
                        resolved_peripherals.push(peripheral_array.clone());
                    }
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD peripheral: {:?}",
                    derived_from
                )));
            };
        } else {
            match device_peripheral {
                svd_rs::MaybeArray::Single(original_peripheral) => {
                    resolved_peripherals.push(original_peripheral.clone())
                }
                svd_rs::MaybeArray::Array(peripheral_array, _) => {
                    log::warn!("Unsupported Array in SVD for Peripheral:{}. Only the first instance will be visible.", peripheral_array.name);
                    resolved_peripherals.push(peripheral_array.clone());
                }
            }
        }
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
        if let Some(derived_from) = &peripheral_register.derived_from {
            if let Some(derived_result) = peripheral.get_register(derived_from) {
                match &peripheral_register.derive_from(derived_result) {
                    svd_rs::MaybeArray::Single(derived_register) => {
                        resolved_registers.push(derived_register.clone())
                    }
                    svd_rs::MaybeArray::Array(register_array, _) => {
                        log::warn!("Unsupported Array in SVD for Register:{}. Only the first instance will be visible.", register_array.name);
                        resolved_registers.push(register_array.clone());
                    }
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD register: {:?}",
                    derived_from
                )));
            };
        } else {
            match peripheral_register {
                svd_rs::MaybeArray::Single(original_register) => {
                    resolved_registers.push(original_register.clone())
                }
                svd_rs::MaybeArray::Array(register_array, _) => {
                    log::warn!("Unsupported Array in SVD for Register:{}. Only the first instance will be visible.", register_array.name);
                    resolved_registers.push(register_array.clone());
                }
            }
        }
    }
    Ok(resolved_registers)
}

/// Resolve all the fields of a register through their (optional) `derived_from` field.
pub(crate) fn resolve_fields(register: &RegisterInfo) -> Result<Vec<FieldInfo>, DebuggerError> {
    // TODO: Need to code for the impact of field clusters.
    let mut resolved_fields = vec![];
    for register_field in register.fields() {
        if let Some(derived_from) = &register_field.derived_from {
            if let Some(derived_result) = register.get_field(derived_from) {
                match &register_field.derive_from(derived_result) {
                    svd_rs::MaybeArray::Single(derived_field) => {
                        resolved_fields.push(derived_field.clone())
                    }
                    svd_rs::MaybeArray::Array(field_array, _) => {
                        log::warn!("Unsupported Array in SVD for Field:{}. Only the first instance will be visible.", field_array.name);
                        resolved_fields.push(field_array.clone());
                    }
                }
            } else {
                return Err(DebuggerError::Other(anyhow::anyhow!(
                    "Unable to retrieve 'derived_from' SVD field: {:?}",
                    derived_from
                )));
            };
        } else {
            match register_field {
                svd_rs::MaybeArray::Single(original_field) => {
                    resolved_fields.push(original_field.clone())
                }
                svd_rs::MaybeArray::Array(field_array, _) => {
                    log::warn!("Unsupported Array in SVD for Field:{}. Only the first instance will be visible.", field_array.name);
                    resolved_fields.push(field_array.clone());
                }
            }
        }
    }
    Ok(resolved_fields)
}
