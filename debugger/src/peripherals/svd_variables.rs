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
use svd_parser::{
    self as svd,
    svd::{Access, Device},
    Config,
};

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
                let svd_cache = match svd::parse_with_config(
                    svd_xml,
                    &Config::default().expand(true).ignore_enums(true),
                ) {
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
                debug_adapter.end_progress(progress_id)?;
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
    for peripheral in &peripheral_device.peripherals {
        if let (Some(peripheral_group_name), VariableName::Named(variable_group_name)) =
            (&peripheral.group_name, &peripheral_group_variable.name)
        {
            if variable_group_name != peripheral_group_name {
                // Before we create a new group variable, check if we have one by that name already.
                match svd_cache.get_variable_by_name_and_parent(
                    &VariableName::Named(peripheral_group_name.clone()),
                    Some(device_root_variable.variable_key),
                ) {
                    Some(existing_peripharal_group_variable) => {
                        peripheral_group_variable = existing_peripharal_group_variable
                    }
                    None => {
                        peripheral_group_variable = Variable::new(None, None);
                        peripheral_group_variable.name =
                            VariableName::Named(peripheral_group_name.clone());
                        peripheral_group_variable.type_name =
                            VariableType::Other("Peripheral Group".to_string());
                        peripheral_group_variable.variable_node_type =
                            VariableNodeType::SvdPeripheral;
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
                    }
                };

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
        peripheral_variable.memory_location = VariableLocation::Address(peripheral.base_address);
        peripheral_variable.set_value(probe_rs::debug::VariableValue::Valid(
            peripheral
                .description
                .clone()
                .unwrap_or_else(|| format!("{}", peripheral_variable.name)),
        ));
        peripheral_variable =
            svd_cache.cache_variable(Some(peripheral_parent_key), peripheral_variable, core)?;
        for register in peripheral.all_registers() {
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
                VariableLocation::Address(peripheral.base_address + register.address_offset as u64);
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
            for field in register.fields() {
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
