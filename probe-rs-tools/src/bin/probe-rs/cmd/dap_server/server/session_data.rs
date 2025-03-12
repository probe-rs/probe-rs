use super::{
    configuration::{self, CoreConfig, SessionConfig},
    core_data::{CoreData, CoreHandle},
};
use crate::{
    cmd::dap_server::{
        DebuggerError,
        debug_adapter::{
            dap::{adapter::DebugAdapter, dap_types::Source},
            protocol::ProtocolAdapter,
        },
    },
    util::common_options::OperationError,
};
use anyhow::{Result, anyhow};
use probe_rs::{
    CoreStatus, Session,
    config::{Registry, TargetSelector},
    probe::list::Lister,
};
use probe_rs_debug::{
    DebugRegisters, SourceLocation, debug_info::DebugInfo, exception_handler_for_core,
};
use std::env::set_current_dir;
use time::UtcOffset;

/// The supported breakpoint types
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BreakpointType {
    /// A breakpoint was requested using an instruction address, and usually a result of a user requesting a
    /// breakpoint while in a 'disassembly' view.
    InstructionBreakpoint,
    /// A breakpoint that has a Source, and usually a result of a user requesting a breakpoint while in a 'source' view.
    SourceBreakpoint {
        source: Box<Source>,
        location: SourceLocationScope,
    },
}

/// Breakpoint requests will either be refer to a specific `SourceLocation`, or unspecified, in which case it will refer to
/// all breakpoints for the Source.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SourceLocationScope {
    All,
    Specific(SourceLocation),
}

/// Provide the storage and methods to handle various [`BreakpointType`]
#[derive(Clone, Debug)]
pub struct ActiveBreakpoint {
    pub(crate) breakpoint_type: BreakpointType,
    pub(crate) address: u64,
}

/// SessionData is designed to be similar to [probe_rs::Session], in as much that it provides handles to the [CoreHandle] instances for each of the available [probe_rs::Core] involved in the debug session.
/// To get access to the [CoreHandle] for a specific [probe_rs::Core], the
/// TODO: Adjust [SessionConfig] to allow multiple cores (and if appropriate, their binaries) to be specified.
pub(crate) struct SessionData {
    pub(crate) session: Session,
    /// [SessionData] will manage one [CoreData] per target core, that is also present in [SessionConfig::core_configs]
    pub(crate) core_data: Vec<CoreData>,

    /// Offset used for RTC timestamps
    ///
    /// Getting the offset can fail, so it's better to store it.
    timestamp_offset: UtcOffset,
}

impl SessionData {
    pub(crate) fn new(
        registry: &mut Registry,
        lister: &Lister,
        config: &mut configuration::SessionConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<Self, DebuggerError> {
        let target_selector = TargetSelector::from(config.chip.as_deref());

        let options = config.probe_options().load(registry)?;
        let target_probe = options.attach_probe(lister)?;
        let target_session = options
            .attach_session(target_probe, target_selector)
            .map_err(|operation_error| {
                match operation_error {
                    OperationError::AttachingFailed {
                        source,
                        connect_under_reset,
                    } => match source {
                        probe_rs::Error::Timeout => {
                            let shared_cause = "This can happen if the target is in a state where it can not be attached to. A hard reset during attach usually helps. For probes that support this option, please try using the `connect_under_reset` option.";
                            if !connect_under_reset {
                                DebuggerError::UserMessage(format!("{source} {shared_cause}"))
                            } else {
                                DebuggerError::UserMessage(format!("{source} {shared_cause} It is possible that your probe does not support this behaviour, or something else is preventing the attach. Please try again without `connect_under_reset`."))
                            }
                        }
                        other_attach_error => other_attach_error.into(),
                    },
                    // Return the orginal error.
                    other => other.into(),
                }
            })?;

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
            return Err(DebuggerError::Other(anyhow!(
                "probe-rs-debugger requires that one, and only one, core be configured for debugging."
            )));
        }

        // Filter `CoreConfig` entries based on those that match an actual core on the target probe.
        let valid_core_configs = config.core_configs.iter().filter(|&core_config| {
            target_session
                .list_cores()
                .iter()
                .any(|(target_core_index, _)| *target_core_index == core_config.core_index)
        });

        let mut core_data_vec = vec![];

        for core_configuration in valid_core_configs {
            core_data_vec.push(CoreData {
                core_index: core_configuration.core_index,
                last_known_status: CoreStatus::Unknown,
                target_name: format!(
                    "{}-{}",
                    core_configuration.core_index,
                    target_session.target().name
                ),
                debug_info: debug_info_from_binary(core_configuration)?,
                static_variables: None,
                core_peripherals: None,
                stack_frames: vec![],
                breakpoints: vec![],
                rtt_connection: None,
                rtt_client: None,
            })
        }

        Ok(SessionData {
            session: target_session,
            core_data: core_data_vec,
            timestamp_offset,
        })
    }

    /// Reload the a specific core's debug info from the binary file.
    pub(crate) fn load_debug_info_for_core(
        &mut self,
        core_configuration: &CoreConfig,
    ) -> Result<(), DebuggerError> {
        if let Some(core_data) = self
            .core_data
            .iter_mut()
            .find(|core_data| core_data.core_index == core_configuration.core_index)
        {
            core_data.debug_info = debug_info_from_binary(core_configuration)?;
            Ok(())
        } else {
            Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            )))
        }
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
                core_data,
            })
        } else {
            Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            )))
        }
    }

    /// The target has no way of notifying the debug adapter when things changes, so we have to constantly poll it to determine:
    /// - Whether the target cores are running, and what their actual status is.
    /// - Whether the target cores have data in their RTT buffers that we need to read and pass to the client.
    ///
    /// To optimize this polling process while also optimizing the reading of RTT data, we apply a couple of principles:
    /// 1. Sleep (nap for a short duration) between polling each target core, but:
    /// - Only sleep IF the core's status hasn't changed AND there was no RTT data in the last poll.
    /// - Otherwise move on without delay, to keep things flowing as fast as possible.
    /// - The justification is that any client side CPU used to keep polling is a small price to pay for maximum throughput of debug requests and RTT from the probe.
    /// 2. Check all target cores to ensure they have a configured and initialized RTT connections and if they do, process the RTT data.
    /// - To keep things efficient, the polling of RTT data is done only when we expect there to be data available.
    /// - We check for RTT only when the core has an RTT connection configured, and one of the following is true:
    ///   - While the core is NOT halted, because core processing can generate new data at any time.
    ///   - The first time we have entered halted status, to ensure the buffers are drained. After that, for as long as we remain in halted state, we don't need to check RTT again.
    ///
    /// Return a Vec of [`CoreStatus`] (one entry per core) after this process has completed, as well as a boolean indicating whether we should consider a short delay before the next poll.
    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) fn poll_cores<P: ProtocolAdapter>(
        &mut self,
        session_config: &SessionConfig,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<(Vec<CoreStatus>, bool), DebuggerError> {
        // By default, we will have a small delay between polls, and will disable it if we know the last poll returned data, on the assumption that there might be at least one more batch of data.
        let mut suggest_delay_required = true;
        let mut status_of_cores: Vec<CoreStatus> = vec![];

        let timestamp_offset = self.timestamp_offset;

        let cores_halted_previously = debug_adapter.all_cores_halted;

        // Always set `all_cores_halted` to true, until one core is found to be running.
        debug_adapter.all_cores_halted = true;
        for core_config in session_config.core_configs.iter() {
            let Ok(mut target_core) = self.attach_core(core_config.core_index) else {
                tracing::debug!(
                    "Failed to attach to target core #{}. Cannot poll for RTT data.",
                    core_config.core_index
                );
                continue;
            };

            // We need to poll the core to determine its status.
            let current_core_status =
                target_core.poll_core(debug_adapter).inspect_err(|error| {
                    let _ = debug_adapter.show_error_message(error);
                })?;

            // If appropriate, check for RTT data.
            if core_config.rtt_config.enabled {
                if let Some(core_rtt) = &mut target_core.core_data.rtt_connection {
                    // We should poll the target for rtt data, and if any RTT data was processed, we clear the flag.
                    if core_rtt.process_rtt_data(debug_adapter, &mut target_core.core) {
                        suggest_delay_required = false;
                    }
                } else if debug_adapter.configuration_is_done() {
                    // We have not yet reached the point in the target application where the RTT buffers are initialized,
                    // so, provided we have processed the MSDAP request for "configurationDone", we should check again.

                    #[allow(clippy::unwrap_used)]
                    match target_core.attach_to_rtt(
                        debug_adapter,
                        core_config.program_binary.as_ref().unwrap(),
                        &core_config.rtt_config,
                        timestamp_offset,
                    ) {
                        Ok(_) => {} // Nothing else to do.
                        Err(error) => {
                            debug_adapter
                                .show_error_message(&DebuggerError::Other(error))
                                .ok();
                        }
                    }
                }
            }

            // If the core is running, we set the flag to indicate that at least one core is not halted.
            // By setting it here, we ensure that RTT will be checked at least once after the core has halted.
            if !current_core_status.is_halted() {
                debug_adapter.all_cores_halted = false;
            // If currently halted, and was previously running
            // update the stack frames
            } else if !cores_halted_previously {
                let _stackframe_span = tracing::debug_span!("Update Stack Frames").entered();
                tracing::debug!(
                    "Updating the stack frame data for core #{}",
                    target_core.core.id()
                );

                let initial_registers = DebugRegisters::from_core(&mut target_core.core);
                let exception_interface = exception_handler_for_core(target_core.core.core_type());
                let instruction_set = target_core.core.instruction_set().ok();

                target_core.core_data.static_variables =
                    Some(target_core.core_data.debug_info.create_static_scope_cache());

                target_core.core_data.stack_frames = target_core.core_data.debug_info.unwind(
                    &mut target_core.core,
                    initial_registers,
                    exception_interface.as_ref(),
                    instruction_set,
                )?;
            }
            status_of_cores.push(current_core_status);
        }
        Ok((status_of_cores, suggest_delay_required))
    }

    pub(crate) fn clean_up(&mut self, session_config: &SessionConfig) -> Result<(), DebuggerError> {
        for core_config in session_config.core_configs.iter() {
            if core_config.rtt_config.enabled {
                let Ok(mut target_core) = self.attach_core(core_config.core_index) else {
                    tracing::debug!(
                        "Failed to attach to target core #{}. Cannot clean up.",
                        core_config.core_index
                    );
                    continue;
                };

                if let Some(core_rtt) = &mut target_core.core_data.rtt_connection {
                    core_rtt.clean_up(&mut target_core.core)?;
                }
            }
        }

        Ok(())
    }
}

fn debug_info_from_binary(core_configuration: &CoreConfig) -> anyhow::Result<DebugInfo> {
    let Some(ref binary_path) = core_configuration.program_binary else {
        return Err(anyhow!(
            "Please provide a valid `program_binary` for debug core: {}",
            core_configuration.core_index
        ));
    };

    DebugInfo::from_file(binary_path).map_err(|error| anyhow!(error))
}
