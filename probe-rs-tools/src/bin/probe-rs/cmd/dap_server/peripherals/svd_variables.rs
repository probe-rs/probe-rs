use crate::cmd::dap_server::{
    debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
    DebuggerError,
};
use std::{fmt::Debug, fs::File, io::Read, path::Path};
use svd_parser::Config;

use super::svd_cache::{SvdVariable, SvdVariableCache};

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
            format!("Loading SVD file: {}", svd_file.display()).as_str(),
            Some(dap_request_id),
        )?;

        let _ = svd_opened_file.read_to_string(svd_xml)?;

        let svd_cache = match svd_parser::parse_with_config(
            svd_xml,
            &Config::default().expand(true).ignore_enums(true),
        ) {
            Ok(peripheral_device) => {
                debug_adapter
                    .update_progress(
                        None,
                        Some(format!("Done loading SVD file: {}", svd_file.display())),
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

/// Create a [`SvdVariableCache`] from a Device that was parsed from a CMSIS-SVD file.
#[tracing::instrument(skip_all)]
pub(crate) fn variable_cache_from_svd<P: ProtocolAdapter>(
    peripheral_device: svd_parser::svd::Device,
    debug_adapter: &mut DebugAdapter<P>,
    progress_id: i64,
) -> Result<SvdVariableCache, DebuggerError> {
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
            match svd_cache
                .get_variable_by_name_and_parent(peripheral_group_name, device_root_variable_key)
            {
                Some(existing_peripheral_group_variable) => {
                    peripheral_parent_key = existing_peripheral_group_variable.variable_key();
                }
                None => {
                    peripheral_parent_key = svd_cache.add_variable(
                        device_root_variable_key,
                        peripheral_group_name.clone(),
                        SvdVariable::SvdPeripheralGroup {
                            description: peripheral.description.clone(),
                        },
                    )?;
                }
            };

            debug_adapter
                .update_progress(
                    None,
                    Some(format!(
                        "SVD loading peripheral group: {peripheral_group_name}",
                    )),
                    progress_id,
                )
                .ok();
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

            let mut register_has_restricted_read = register.read_action.is_some()
                || register
                    .properties
                    .access
                    .map(|a| !a.can_read())
                    .or_else(|| device_default_access.map(|a| !a.can_read()))
                    .unwrap_or(true);

            let register_name = format!("{}.{}", &peripheral_name, register.name);

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
                format!("{}.{}", &peripheral_name, register.name),
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
