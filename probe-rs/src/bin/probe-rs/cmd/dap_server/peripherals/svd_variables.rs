use crate::cmd::dap_server::{
    debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
    DebuggerError,
};
use std::{fmt::Debug, fs::File, io::Read, path::Path};
use svd_parser::{
    self as svd,
    svd::{Access, Device},
    Config,
};

use super::svd_cache::{
    SvdVariable, SvdVariableCache, SvdVariableName, SvdVariableNodeType, SvdVariableType,
    SvdVariableValue,
};

/// The SVD file contents and related data
#[derive(Debug)]
pub struct SvdCache {
    /// The SVD contents and structure will be stored as variables, down to the Field level.
    /// Unlike other VariableCache instances, it will only be built once per DebugSession.
    /// After that, only the SVD fields values change values, and the data for these will be re-read
    /// every time they are queried by the debugger.
    pub(crate) svd_variable_cache: SvdVariableCache,
}

impl SvdCache {
    /// Create the SVD cache for a specific core. This function loads the file, parses it, and then builds the VariableCache.
    pub(crate) fn new<P: ProtocolAdapter>(
        svd_file: &Path,
        debug_adapter: &mut DebugAdapter<P>,
        dap_request_id: i64,
    ) -> Result<Self, DebuggerError> {
        let svd_xml = &mut String::new();

        let mut svd_opened_file = File::open(svd_file)?;

        let progress_id = debug_adapter.start_progress(
            format!("Loading SVD file : {}", svd_file.display()).as_str(),
            Some(dap_request_id),
        )?;

        let _ = svd_opened_file.read_to_string(svd_xml)?;

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
}

/// Create a [`probe_rs::debug::SvdVariableCache`] from a Device that was parsed from a CMSIS-SVD file.
pub(crate) fn variable_cache_from_svd<P: ProtocolAdapter>(
    peripheral_device: Device,
    debug_adapter: &mut DebugAdapter<P>,
    progress_id: i64,
) -> Result<SvdVariableCache, DebuggerError> {
    let mut svd_cache = SvdVariableCache::new_svd_cache();
    let device_root_variable = svd_cache.root_variable();

    // Adding the Peripheral Group Name as an additional level in the structure helps to keep the 'variable tree' more compact,
    // but more importantly, it helps to avoid having duplicate variable names that conflict with hal crates.
    let mut peripheral_group_variable = SvdVariable::new(
        SvdVariableName::Named(peripheral_device.name.clone()),
        SvdVariableType::PeripheralGroup,
    );
    let mut peripheral_parent_key = device_root_variable.variable_key();

    for peripheral in &peripheral_device.peripherals {
        if let (Some(peripheral_group_name), SvdVariableName::Named(variable_group_name)) =
            (&peripheral.group_name, &peripheral_group_variable.name)
        {
            if variable_group_name != peripheral_group_name {
                // Before we create a new group variable, check if we have one by that name already.
                match svd_cache.get_variable_by_name_and_parent(
                    &SvdVariableName::Named(peripheral_group_name.clone()),
                    device_root_variable.variable_key(),
                ) {
                    Some(existing_peripharal_group_variable) => {
                        peripheral_group_variable = existing_peripharal_group_variable
                    }
                    None => {
                        peripheral_group_variable = SvdVariable::new(
                            SvdVariableName::Named(peripheral_group_name.clone()),
                            SvdVariableType::PeripheralGroup,
                        );

                        peripheral_group_variable.variable_node_type =
                            SvdVariableNodeType::SvdPeripheralGroup;
                        peripheral_group_variable.set_value(SvdVariableValue::Fixed(
                            peripheral
                                .description
                                .clone()
                                .unwrap_or_else(|| peripheral.name.clone()),
                        ));
                        svd_cache.add_variable(
                            device_root_variable.variable_key(),
                            &mut peripheral_group_variable,
                        )?;
                    }
                };

                peripheral_parent_key = peripheral_group_variable.variable_key();
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

        let mut peripheral_variable = SvdVariable::new(
            SvdVariableName::Named(format!(
                "{}.{}",
                peripheral_group_variable.name, peripheral.name
            )),
            SvdVariableType::Peripheral,
        );
        peripheral_variable.variable_node_type = SvdVariableNodeType::SvdPeripheral {
            base_address: peripheral.base_address,
        };
        peripheral_variable.set_value(SvdVariableValue::Fixed(
            peripheral
                .description
                .clone()
                .unwrap_or_else(|| format!("{}", peripheral_variable.name)),
        ));
        svd_cache.add_variable(peripheral_parent_key, &mut peripheral_variable)?;

        for register in peripheral.all_registers() {
            let mut register_variable = SvdVariable::new(
                SvdVariableName::Named(format!(
                    "{}.{}",
                    &peripheral_variable.name,
                    register.name.clone()
                )),
                SvdVariableType::Other(
                    register
                        .description
                        .clone()
                        .unwrap_or_else(|| "Peripheral Register".to_string()),
                ),
            );

            let register_address = peripheral.base_address + register.address_offset as u64;

            register_variable.variable_node_type =
                SvdVariableNodeType::SvdRegister(register_address);

            let mut register_has_restricted_read = false;
            if register.read_action.is_some()
                || (if let Some(register_access) = register.properties.access {
                    register_access == Access::ReadWriteOnce || register_access == Access::WriteOnly
                } else {
                    false
                })
            {
                register_variable.set_value(SvdVariableValue::Error(
                    "Register access doesn't allow reading, or will have side effects.".to_string(),
                ));
                register_has_restricted_read = true;
            }
            svd_cache.add_variable(peripheral_variable.variable_key(), &mut register_variable)?;

            for field in register.fields() {
                let mut field_variable = SvdVariable::new(
                    SvdVariableName::Named(format!(
                        "{}.{}",
                        &register_variable.name,
                        field.name.clone()
                    )),
                    SvdVariableType::Other(
                        field
                            .description
                            .clone()
                            .unwrap_or_else(|| "Register Field".to_string()),
                    ),
                );
                field_variable.variable_node_type = SvdVariableNodeType::SvdField {
                    address: register_address,
                    bit_range_lower_bound: field.bit_offset() as i64,
                    bit_range_upper_bound: (field.bit_offset() + field.bit_width()) as i64,
                };
                if register_has_restricted_read {
                    register_variable.set_value(SvdVariableValue::Error(
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
                    field_variable.set_value(SvdVariableValue::Error(
                        "Field access doesn't allow reading, or will have side effects."
                            .to_string(),
                    ));
                    // If we can't read any of the bits, then don't read the register either.
                    register_variable.set_value(SvdVariableValue::Error(
                        "Some fields' access doesn't allow reading, or will have side effects."
                            .to_string(),
                    ));
                    register_has_restricted_read = true;
                    svd_cache.update_variable(&register_variable)?;
                }
                // TODO: Extend the Variable definition, so that we can resolve the EnumeratedValues for fields.
                svd_cache.add_variable(register_variable.variable_key(), &mut field_variable)?;
            }
        }
    }

    Ok(svd_cache)
}
