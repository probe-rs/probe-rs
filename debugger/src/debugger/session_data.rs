use super::configuration;
use super::core_data::CoreData;
use crate::debugger::debug_rtt;
use crate::DebuggerError;
use anyhow::{anyhow, Result};
use capstone::Endian;
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::riscv::ArchMode as riscvArchMode, prelude::*,
    Capstone,
};
use probe_rs::config::TargetSelector;
use probe_rs::debug::DebugInfo;
use probe_rs::DebugProbeError;
use probe_rs::Permissions;
use probe_rs::Probe;
use probe_rs::ProbeCreationError;
use probe_rs::Session;
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

/// SessionData is designed to be similar to [probe_rs::Session], in as much that it provides handles to the [CoreData] instances for each of the available [probe_rs::Core] involved in the debug session.
/// To get access to the [CoreData] for a specific [Core], the
/// TODO: Adjust [SessionConfig] to allow multiple cores (and if appropriate, their binaries) to be specified.
pub struct SessionData {
    pub(crate) session: Session,
    /// Provides ability to disassemble binary code.
    pub(crate) capstone: Capstone,
    /// [SessionData] will manage one [DebugInfo] per [SessionConfig::program_binary]
    pub(crate) debug_infos: Vec<DebugInfo>,
    /// [SessionData] will manage a `Vec<StackFrame>` per [Core]. Each core's collection of StackFrames will be recreated whenever a stacktrace is performed, using the results of [DebugInfo::unwind]
    pub(crate) stack_frames: Vec<Vec<probe_rs::debug::StackFrame>>,
    /// [SessionData] will manage a `Vec<ActiveBreakpoint>` per [Core]. Each core's collection of ActiveBreakpoint's will be managed on demand.
    pub(crate) breakpoints: Vec<Vec<ActiveBreakpoint>>,
    /// The control structures for handling RTT in this Core of the SessionData.
    pub(crate) rtt_connection: Option<debug_rtt::RttConnection>,
}

impl SessionData {
    pub(crate) fn new(config: &configuration::SessionConfig) -> Result<Self, DebuggerError> {
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
        if let Some(protocol) = config.protocol {
            target_probe.select_protocol(protocol)?;
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

        // TODO: We currently only allow a single core & binary to be specified in [SessionConfig]. When this is extended to support multicore, the following should initialize [DebugInfo] and [VariableCache] for each available core.
        // Configure the [DebugInfo].
        let debug_infos = vec![if let Some(binary_path) = &config.program_binary {
            DebugInfo::from_file(binary_path)
                .map_err(|error| DebuggerError::Other(anyhow!(error)))?
        } else {
            return Err(
                anyhow!("Please provide a valid `program_binary` for this debug session").into(),
            );
        }];

        // Configure the [VariableCache].
        let stack_frames = target_session.list_cores()
            .iter()
            .map(|(core_id, core_type)| {
                log::debug!(
                    "Preparing stack frame variable cache for SessionData and CoreData for core #{} of type: {:?}",
                    core_id,
                    core_type
                );
                Vec::<probe_rs::debug::StackFrame>::new()
            }).collect();

        // Prepare the breakpoint cache
        let breakpoints = target_session.list_cores()
            .iter()
            .map(|(core_id, core_type)| {
                log::debug!(
                    "Preparing breakpoint cache for SessionData and CoreData for core #{} of type: {:?}",
                    core_id,
                    core_type
                );
                Vec::<ActiveBreakpoint>::new()
            }).collect();

        Ok(SessionData {
            session: target_session,
            capstone,
            debug_infos,
            stack_frames,
            breakpoints,
            rtt_connection: None,
        })
    }

    pub fn attach_core(&mut self, core_index: usize) -> Result<CoreData, DebuggerError> {
        let target_name = self.session.target().name.clone();
        // Do a 'light weight'(just get references to existing data structures) attach to the core and return relevant debug data.
        match self.session.core(core_index) {
            Ok(target_core) => Ok(CoreData {
                target_core,
                target_name: format!("{}-{}", core_index, target_name),
                debug_info: self.debug_infos.get(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "No available `DebugInfo` for core # {}",
                        core_index
                    ))
                })?,
                stack_frames: self.stack_frames.get_mut(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "StackFrame cache was not correctly configured for core # {}",
                        core_index
                    ))
                })?,
                capstone: &self.capstone,
                breakpoints: self.breakpoints.get_mut(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "ActiveBreakpoint cache was not correctly configured for core # {}",
                        core_index
                    ))
                })?,
                rtt_connection: &mut self.rtt_connection,
            }),
            Err(_) => Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            ))),
        }
    }
}
