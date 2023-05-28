use crate::{
    architecture::{
        arm::{
            ap::MemoryAp,
            core::{CortexAState, CortexMState},
            memory::adi_v5_memory_interface::ArmProbe,
            ApAddress, DpAddress,
        },
        riscv::{communication_interface::RiscvCommunicationInterface, RiscVState},
    },
    Core, CoreType, Error,
};
pub use probe_rs_target::{Architecture, CoreAccessOptions};

use super::ResolvedCoreOptions;

/// A generic core state which caches the generic parts of the core state.
#[derive(Debug)]
pub struct CoreState {
    id: usize,

    /// Information needed to access the core
    pub(crate) core_access_options: ResolvedCoreOptions,
}

impl CoreState {
    /// Creates a new core state from the core ID.
    pub fn new(id: usize, core_access_options: ResolvedCoreOptions) -> Self {
        Self {
            id,
            core_access_options,
        }
    }

    /// Returns the core ID.
    pub fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn memory_ap(&self) -> MemoryAp {
        let arm_core_access_options = match &self.core_access_options {
            ResolvedCoreOptions::Arm { options, .. } => options,
            ResolvedCoreOptions::Riscv { .. } => {
                panic!("This should never happen. Please file a bug if it does.")
            }
        };

        let dp = match arm_core_access_options.psel {
            0 => DpAddress::Default,
            x => DpAddress::Multidrop(x),
        };

        let ap = ApAddress {
            dp,
            ap: arm_core_access_options.ap,
        };

        MemoryAp::new(ap)
    }
}

/// The architecture specific core state.
#[derive(Debug)]
pub enum SpecificCoreState {
    /// The state of an ARMv6-M core.
    Armv6m(CortexMState),
    /// The state of an ARMv7-A core.
    Armv7a(CortexAState),
    /// The state of an ARMv7-M core.
    Armv7m(CortexMState),
    /// The state of an ARMv7-EM core.
    Armv7em(CortexMState),
    /// The state of an ARMv8-A core.
    Armv8a(CortexAState),
    /// The state of an ARMv8-M core.
    Armv8m(CortexMState),
    /// The state of an RISC-V core.
    Riscv(RiscVState),
}

impl SpecificCoreState {
    pub(crate) fn from_core_type(typ: CoreType) -> Self {
        match typ {
            CoreType::Armv6m => SpecificCoreState::Armv6m(CortexMState::new()),
            CoreType::Armv7a => SpecificCoreState::Armv7a(CortexAState::new()),
            CoreType::Armv7m => SpecificCoreState::Armv7m(CortexMState::new()),
            CoreType::Armv7em => SpecificCoreState::Armv7m(CortexMState::new()),
            CoreType::Armv8a => SpecificCoreState::Armv8a(CortexAState::new()),
            CoreType::Armv8m => SpecificCoreState::Armv8m(CortexMState::new()),
            CoreType::Riscv => SpecificCoreState::Riscv(RiscVState::new()),
        }
    }

    pub(crate) fn core_type(&self) -> CoreType {
        match self {
            SpecificCoreState::Armv6m(_) => CoreType::Armv6m,
            SpecificCoreState::Armv7a(_) => CoreType::Armv7a,
            SpecificCoreState::Armv7m(_) => CoreType::Armv7m,
            SpecificCoreState::Armv7em(_) => CoreType::Armv7em,
            SpecificCoreState::Armv8a(_) => CoreType::Armv8a,
            SpecificCoreState::Armv8m(_) => CoreType::Armv8m,
            SpecificCoreState::Riscv(_) => CoreType::Riscv,
        }
    }

    pub(crate) fn attach_arm<'probe, 'target: 'probe>(
        &'probe mut self,
        state: &'probe mut CoreState,
        memory: Box<dyn ArmProbe + 'probe>,
    ) -> Result<Core<'probe>, Error> {
        /*
        let debug_sequence = match &target.debug_sequence {
            crate::config::DebugSequence::Arm(sequence) => sequence.clone(),
            crate::config::DebugSequence::Riscv(_) => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        let options = match &state.core_access_options {
            CoreAccessOptions::Arm(options) => options,
            CoreAccessOptions::Riscv(_) => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        */

        let (options, debug_sequence) = match &state.core_access_options {
            ResolvedCoreOptions::Arm { options, sequence } => (options, sequence.clone()),
            ResolvedCoreOptions::Riscv { .. } => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        Ok(match self {
            SpecificCoreState::Armv6m(s) => Core::new(
                crate::architecture::arm::armv6m::Armv6m::new(memory, s, debug_sequence)?,
                state,
            ),
            SpecificCoreState::Armv7a(s) => Core::new(
                crate::architecture::arm::armv7a::Armv7a::new(
                    memory,
                    s,
                    options.debug_base.expect("base_address not specified"),
                    debug_sequence,
                )?,
                state,
            ),
            SpecificCoreState::Armv7m(s) | SpecificCoreState::Armv7em(s) => Core::new(
                crate::architecture::arm::armv7m::Armv7m::new(memory, s, debug_sequence)?,
                state,
            ),
            SpecificCoreState::Armv8a(s) => Core::new(
                crate::architecture::arm::armv8a::Armv8a::new(
                    memory,
                    s,
                    options.debug_base.expect("base_address not specified"),
                    options.cti_base.expect("cti_address not specified"),
                    debug_sequence,
                )?,
                state,
            ),
            SpecificCoreState::Armv8m(s) => Core::new(
                crate::architecture::arm::armv8m::Armv8m::new(memory, s, debug_sequence)?,
                state,
            ),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub(crate) fn attach_riscv<'probe>(
        &'probe mut self,
        state: &'probe mut CoreState,
        interface: &'probe mut RiscvCommunicationInterface,
    ) -> Result<Core<'probe>, Error> {
        Ok(match self {
            SpecificCoreState::Riscv(s) => Core::new(
                crate::architecture::riscv::Riscv32::new(interface, s),
                state,
            ),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }
}
