use super::{
    configuration::{self, CoreConfig, SessionConfig},
    core_data::{CoreData, CoreHandle},
};
use crate::{
    debug_adapter::{dap_adapter::DebugAdapter, protocol::ProtocolAdapter},
    DebuggerError,
};
use anyhow::{anyhow, Result};
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::riscv::ArchMode as riscvArchMode, prelude::*,
    Capstone, Endian,
};
use probe_rs::{
    config::TargetSelector, debug::debug_info::DebugInfo, CoreStatus, DebugProbeError, Permissions,
    Probe, ProbeCreationError, Session,
};
use std::env::set_current_dir;

/// The supported breakpoint types
#[derive(Debug, PartialEq)]
pub enum BreakpointType {
    InstructionBreakpoint,
    SourceBreakpoint,
}

/// Provide the storage and methods to handle various [`BreakPointType`]
#[derive(Debug)]
pub struct ActiveBreakpoint {
    pub(crate) breakpoint_type: BreakpointType,
    pub(crate) breakpoint_address: u32,
}

/// SessionData is designed to be similar to [probe_rs::Session], in as much that it provides handles to the [CoreHandle] instances for each of the available [probe_rs::Core] involved in the debug session.
/// To get access to the [CoreHandle] for a specific [Core], the
/// TODO: Adjust [SessionConfig] to allow multiple cores (and if appropriate, their binaries) to be specified.
pub struct SessionData {
    pub(crate) session: Session,
    /// Provides ability to disassemble binary code.
    pub(crate) capstone: Capstone,
    /// [SessionData] will manage one [CoreData] per target core, that is also present in [SessionConfig::core_configs]
    pub(crate) core_data: Vec<CoreData>,
}

impl SessionData {
    pub(crate) fn new(config: &configuration::SessionConfig) -> Result<Self, DebuggerError> {
        // `SessionConfig` Probe/Session level configurations initialization.
        let mut target_probe = match config.probe_selector.clone() {
            Some(selector) => Probe::open(selector.clone()).map_err(|e| match e {
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound) => {
                    DebuggerError::Other(anyhow!(
                        "Could not find the probe_selector specified as {:04x}:{:04x}:{:?}",
                        selector.vendor_id,
                        selector.product_id,
                        selector.serial_number
                    ))
                }
                other_error => DebuggerError::DebugProbe(other_error),
            }),
            None => {
                // Only automatically select a probe if there is only a single probe detected.
                let list = Probe::list_all();
                if list.len() > 1 {
                    return Err(DebuggerError::Other(anyhow!(
                        "Found multiple ({}) probes",
                        list.len()
                    )));
                }

                if let Some(info) = list.first() {
                    Probe::open(info).map_err(DebuggerError::DebugProbe)
                } else {
                    return Err(DebuggerError::Other(anyhow!(
                        "No probes found. Please check your USB connections."
                    )));
                }
            }
        }?;

        let target_selector = match &config.chip {
            Some(identifier) => identifier.into(),
            None => TargetSelector::Auto,
        };

        // Set the protocol, if the user explicitly selected a protocol. Otherwise, use the default protocol of the probe.
        if let Some(wire_protocol) = config.wire_protocol {
            target_probe.select_protocol(wire_protocol)?;
        }

        // Set the speed.
        if let Some(speed) = config.speed {
            let actual_speed = target_probe.set_speed(speed)?;
            if actual_speed != speed {
                log::warn!(
                    "Protocol speed {} kHz not supported, actual speed is {} kHz",
                    speed,
                    actual_speed
                );
            }
        }

        let mut permissions = Permissions::new();
        if config.allow_erase_all {
            permissions = permissions.allow_erase_all();
        }

        // Attach to the probe.
        let target_session = if config.connect_under_reset {
            target_probe.attach_under_reset(target_selector, permissions)?
        } else {
            target_probe
                .attach(target_selector, permissions)
                .map_err(|err| {
                    anyhow!(
                        "Error attaching to the probe: {:?}.\nTry the --connect-under-reset option",
                        err
                    )
                })?
        };

        // Create an instance of the [`capstone::Capstone`] for disassembly capabilities.
        // TODO: I believe it is safe to share this between multiple cores, but needs to be tested.
        let capstone = match target_session.architecture() {
            probe_rs::Architecture::Arm => Capstone::new()
                .arm()
                .mode(armArchMode::Thumb)
                .endian(Endian::Little)
                .build()
                .map_err(|err| anyhow!("Error creating Capstone disassembler: {:?}", err))?,
            probe_rs::Architecture::Riscv => Capstone::new()
                .riscv()
                .mode(riscvArchMode::RiscV32)
                .endian(Endian::Little)
                .build()
                .map_err(|err| anyhow!("Error creating Capstone disassembler: {:?}", err))?,
        };

        // Change the current working directory if `config.cwd` is `Some(T)`.
        if let Some(new_cwd) = config.cwd.clone() {
            set_current_dir(new_cwd.as_path()).map_err(|err| {
                anyhow!(
                    "Failed to set current working directory to: {:?}, {:?}",
                    new_cwd,
                    err
                )
            })?;
        };

        // `FlashingConfig` probe level initialization.

        // `CoreConfig` probe level initialization.
        if config.core_configs.len() != 1 {
            // TODO: For multi-core, allow > 1.
            return Err(DebuggerError::Other(anyhow!("probe-rs-debugger requires that one, and only one, core  be configured for debugging.")));
        }

        // Filter `CoreConfig` entries based on those that match an actual core on the target probe.
        let valid_core_configs = config
            .core_configs
            .iter()
            .filter(|&core_config| {
                matches!(
                        target_session
                            .list_cores()
                            .iter()
                            .find(|(target_core_index, _)| *target_core_index
                                == core_config.core_index),
                        Some(_)
                    )
            })
            .cloned()
            .collect::<Vec<CoreConfig>>();

        let mut core_data_vec = vec![];

        for core_configuration in &valid_core_configs {
            // Configure the [DebugInfo].
            let debug_info = if let Some(binary_path) = &core_configuration.program_binary {
                DebugInfo::from_file(binary_path)
                    .map_err(|error| DebuggerError::Other(anyhow!(error)))?
            } else {
                return Err(anyhow!(
                    "Please provide a valid `program_binary` for debug core: {:?}",
                    core_configuration.core_index
                )
                .into());
            };

            core_data_vec.push(CoreData {
                core_index: core_configuration.core_index,
                target_name: format!(
                    "{}-{}",
                    core_configuration.core_index,
                    target_session.target().name
                ),
                debug_info,
                core_peripherals: None,
                stack_frames: Vec::<probe_rs::debug::stack_frame::StackFrame>::new(),
                breakpoints: Vec::<ActiveBreakpoint>::new(),
                rtt_connection: None,
            })
        }

        Ok(SessionData {
            session: target_session,
            capstone,
            core_data: core_data_vec,
        })
    }

    /// Do a 'light weight'(just get references to existing data structures) attach to the core and return relevant debug data.
    pub(crate) fn attach_core(&mut self, core_index: usize) -> Result<CoreHandle, DebuggerError> {
        if let (Ok(target_core), Some(core_data)) = (
            self.session.core(core_index),
            self.core_data
                .iter_mut()
                .find(|core_data| core_data.core_index == core_index),
        ) {
            Ok(CoreHandle {
                core: target_core,
                capstone: &self.capstone,
                core_data,
            })
        } else {
            Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            )))
        }
    }

    /// Check all target cores to ensure they have a configured and initialized RTT connections and if they do, process the RTT data.
    /// Return true if at least one channel on one core had data in the buffer.
    pub(crate) fn poll_rtt<P: ProtocolAdapter>(
        &mut self,
        session_config: &SessionConfig,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> bool {
        let mut at_least_one_channel_had_data = false;
        for core_config in session_config.core_configs.iter() {
            if core_config.rtt_config.enabled {
                let target_memory_map = self.session.target().memory_map.clone();
                if let Ok(mut target_core) = self.attach_core(core_config.core_index) {
                    if let Some(core_rtt) = &mut target_core.core_data.rtt_connection {
                        // We should poll the target for rtt data.
                        at_least_one_channel_had_data |=
                            core_rtt.process_rtt_data(debug_adapter, &mut target_core.core);
                    } else {
                        // We have not yet reached the point in the target application where the RTT buffers are initialized, so let's check again.
                        if debug_adapter.last_known_status != CoreStatus::Unknown
                        // Do not attempt this until we have processed the MSDAP request for "configurationDone" ...
                        {
                            #[allow(clippy::unwrap_used)]
                            match target_core.attach_to_rtt(
                                debug_adapter,
                                &target_memory_map,
                                core_config.program_binary.as_ref().unwrap(),
                                &core_config.rtt_config,
                            ) {
                                Ok(_) => {
                                    // Nothing else to do.
                                }
                                Err(error) => {
                                    debug_adapter
                                        .send_error_response(&DebuggerError::Other(error))
                                        .ok();
                                }
                            }
                        }
                    }
                } else {
                    log::debug!(
                        "Failed to attach to target core #{}. Cannot poll for RTT data.",
                        core_config.core_index
                    );
                }
            } else {
                // No RTT configured.
            }
        }
        at_least_one_channel_had_data
    }
}
