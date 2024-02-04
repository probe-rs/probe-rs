use crate::{
    architecture::{
        arm::{
            ap::MemoryAp,
            core::{CortexAState, CortexMState},
            memory::adi_v5_memory_interface::ArmProbe,
            ApAddress, ArmProbeInterface, DpAddress,
        },
        riscv::{communication_interface::RiscvCommunicationInterface, RiscVState},
        xtensa::{communication_interface::XtensaCommunicationInterface, XtensaState},
    },
    Core, CoreType, Error, Target,
};

use super::ResolvedCoreOptions;

#[derive(Debug)]
pub(crate) struct CombinedCoreState {
    /// Flag to indicate if the core is enabled for debugging
    ///
    /// In multi-core systems, only a subset of cores could be enabled.
    pub(crate) debug_enabled: bool,

    pub(crate) core_state: CoreState,

    pub(crate) specific_state: SpecificCoreState,

    pub(crate) id: usize,
}

impl CombinedCoreState {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn core_type(&self) -> CoreType {
        self.specific_state.core_type()
    }

    pub(crate) fn attach_arm<'probe>(
        &'probe mut self,
        target: &'probe Target,
        arm_interface: &'probe mut Box<dyn ArmProbeInterface>,
    ) -> Result<Core<'probe>, Error> {
        let memory_regions = &target.memory_map;

        let name = &target.cores[self.id].name;

        let memory = arm_interface.memory_interface(self.arm_memory_ap())?;

        let (options, debug_sequence) = match &self.core_state.core_access_options {
            ResolvedCoreOptions::Arm { options, sequence } => (options, sequence.clone()),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        Ok(match &mut self.specific_state {
            SpecificCoreState::Armv6m(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::arm::armv6m::Armv6m::new(memory, s, debug_sequence)?,
            ),
            SpecificCoreState::Armv7a(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::arm::armv7a::Armv7a::new(
                    memory,
                    s,
                    options.debug_base.expect("base_address not specified"),
                    debug_sequence,
                )?,
            ),
            SpecificCoreState::Armv7m(s) | SpecificCoreState::Armv7em(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::arm::armv7m::Armv7m::new(memory, s, debug_sequence)?,
            ),
            SpecificCoreState::Armv8a(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::arm::armv8a::Armv8a::new(
                    memory,
                    s,
                    options.debug_base.expect("base_address not specified"),
                    options.cti_base.expect("cti_address not specified"),
                    debug_sequence,
                )?,
            ),
            SpecificCoreState::Armv8m(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::arm::armv8m::Armv8m::new(memory, s, debug_sequence)?,
            ),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub(crate) fn enable_arm_debug(
        &self,
        interface: &mut dyn ArmProbeInterface,
    ) -> Result<(), Error> {
        let ResolvedCoreOptions::Arm { sequence, options } = &self.core_state.core_access_options
        else {
            unreachable!("This should never happen. Please file a bug if it does.");
        };

        tracing::debug_span!("debug_core_start", id = self.id()).in_scope(|| {
            // Enable debug mode
            sequence.debug_core_start(
                interface,
                self.arm_memory_ap(),
                self.core_type(),
                options.debug_base,
                options.cti_base,
            )
        })?;

        Ok(())
    }

    pub(crate) fn arm_reset_catch_set(
        &self,
        interface: &mut dyn ArmProbeInterface,
    ) -> Result<(), Error> {
        let ResolvedCoreOptions::Arm { sequence, options } = &self.core_state.core_access_options
        else {
            unreachable!("This should never happen. Please file a bug if it does.");
        };

        let mut memory_interface = interface.memory_interface(self.arm_memory_ap())?;

        let reset_catch_span = tracing::debug_span!("reset_catch_set", id = self.id()).entered();
        sequence.reset_catch_set(&mut *memory_interface, self.core_type(), options.debug_base)?;

        drop(reset_catch_span);

        Ok(())
    }

    pub(crate) fn attach_riscv<'probe>(
        &'probe mut self,
        target: &'probe Target,
        interface: &'probe mut RiscvCommunicationInterface,
    ) -> Result<Core<'probe>, Error> {
        let memory_regions = &target.memory_map;
        let name = &target.cores[self.id].name;

        let options = match &self.core_state.core_access_options {
            ResolvedCoreOptions::Riscv { options } => options,
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        Ok(match &mut self.specific_state {
            SpecificCoreState::Riscv(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::riscv::Riscv32::new(
                    options.hart_id.unwrap_or_default(),
                    interface,
                    s,
                )?,
            ),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub(crate) fn attach_xtensa<'probe>(
        &'probe mut self,
        target: &'probe Target,
        interface: &'probe mut XtensaCommunicationInterface,
    ) -> Result<Core<'probe>, Error> {
        let memory_regions = &target.memory_map;
        let name = &target.cores[self.id].name;

        Ok(match &mut self.specific_state {
            SpecificCoreState::Xtensa(s) => Core::new(
                self.id,
                name,
                memory_regions,
                crate::architecture::xtensa::Xtensa::new(interface, s),
            ),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    /// Get the memory AP for this core.
    ///
    /// ## Panic
    ///
    /// This function will panic if the core is not an ARM core and doesn't have a memory AP
    pub(crate) fn arm_memory_ap(&self) -> MemoryAp {
        self.core_state.memory_ap()
    }

    pub(crate) fn enable_debug(
        &self,
        interface: &mut crate::session::ArchitectureInterface,
    ) -> Result<(), Error> {
        match interface {
            crate::session::ArchitectureInterface::Arm(interface) => {
                let mut interface: &mut dyn ArmProbeInterface = interface.as_mut();
                self.enable_arm_debug(interface)
            }
            _ => todo!(),
        }
    }

    pub(crate) fn disable_debug(
        &self,
        interface: &mut crate::session::ArchitectureInterface,
    ) -> Result<(), Error> {
        match interface {
            crate::session::ArchitectureInterface::Arm(interface) => {
                let mut interface: &mut dyn ArmProbeInterface = interface.as_mut();
                self.disable_arm_debug(interface)
            }
            _ => todo!(),
        }
    }

    fn disable_arm_debug(&self, interface: &mut dyn ArmProbeInterface) -> Result<(), Error> {
        let ResolvedCoreOptions::Arm { sequence, options } = &self.core_state.core_access_options
        else {
            unreachable!("This should never happen. Please file a bug if it does.");
        };

        let mut memory_interface = interface.memory_interface(self.arm_memory_ap())?;
        let mut memory_interface: &mut dyn ArmProbe = memory_interface.as_mut();

        tracing::debug_span!("debug_core_stop", id = self.id()).in_scope(|| {
            // Enable debug mode
            sequence.debug_core_stop(memory_interface, self.core_type())
        })?;

        Ok(())
    }
}

/// A generic core state which caches the generic parts of the core state.
#[derive(Debug)]
pub struct CoreState {
    id: usize,
    /// Information needed to access the core
    pub core_access_options: ResolvedCoreOptions,
}

impl CoreState {
    /// Creates a new core state from the core ID.
    pub fn new(id: usize, core_access_options: ResolvedCoreOptions) -> Self {
        Self {
            id,
            core_access_options,
        }
    }

    pub(crate) fn memory_ap(&self) -> MemoryAp {
        let arm_core_access_options = match self.core_access_options {
            ResolvedCoreOptions::Arm { ref options, .. } => options,
            _ => unreachable!("This should never happen. Please file a bug if it does."),
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

    pub(crate) fn id(&self) -> usize {
        self.id
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
    /// The state of an Xtensa core.
    Xtensa(XtensaState),
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
            CoreType::Xtensa => SpecificCoreState::Xtensa(XtensaState::new()),
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
            SpecificCoreState::Xtensa(_) => CoreType::Xtensa,
        }
    }

    /*

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
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        };

        Ok(match self {
            SpecificCoreState::Armv6m(s) => Core::new(
                state.id,
                name,
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
        todo!();

        /*
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

        */
    }

    pub(crate) fn attach_xtensa<'probe>(
        &'probe mut self,
        state: &'probe mut CoreState,
        interface: &'probe mut XtensaCommunicationInterface,
    ) -> Result<Core<'probe>, Error> {
        todo!()
    }

    */
}
