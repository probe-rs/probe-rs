//! Debug Module Communication
//!
//! This module implements communication with a
//! Debug Module, as described in the RISC-V debug
//! specification v0.13.2 .

use crate::architecture::riscv::dtm::dtm_access::DtmAccess;
use crate::{
    architecture::riscv::*, memory_mapped_bitfield_register, probe::DeferredResultIndex,
    Error as ProbeRsError,
};
use std::any::Any;
use std::collections::HashMap;

/// Some error occurred when working with the RISC-V core.
#[derive(thiserror::Error, Debug)]
pub enum RiscvError {
    /// An error occurred during transport
    #[error("Error during transport")]
    DtmOperationFailed,
    /// DMI operation is in progress
    #[error("Transport operation in progress")]
    DtmOperationInProcess,
    /// An error with operating the debug probe occurred.
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// A timeout occurred during DMI access.
    #[error("Timeout during DMI access.")]
    Timeout,
    /// An error occurred during the execution of an abstract command.
    #[error("Error occurred during execution of an abstract command: {0:?}")]
    AbstractCommand(AbstractCommandErrorKind),
    /// The request for reset, resume or halt was not acknowledged.
    #[error("The core did not acknowledge a request for reset, resume or halt")]
    RequestNotAcknowledged,
    /// This debug transport module (DTM) version is currently not supported.
    #[error("The version '{0}' of the debug transport module (DTM) is currently not supported.")]
    UnsupportedDebugTransportModuleVersion(u8),
    /// This version of the debug module is not supported.
    #[error("The version '{0:?}' of the debug module is currently not supported.")]
    UnsupportedDebugModuleVersion(DebugModuleVersion),
    /// The provided csr address was invalid/unsupported
    #[error("CSR at address '{0:x}' is unsupported.")]
    UnsupportedCsrAddress(u16),
    /// The given program buffer register is not supported.
    #[error("Program buffer register '{0}' is currently not supported.")]
    UnsupportedProgramBufferRegister(usize),
    /// The program buffer is too small for the supplied program.
    #[error("Program buffer is too small for supplied program.")]
    ProgramBufferTooSmall,
    /// Memory width larger than 32 bits is not supported yet.
    #[error("Memory width larger than 32 bits is not supported yet.")]
    UnsupportedBusAccessWidth(RiscvBusAccess),
    /// An error during system bus access occurred.
    #[error("Error using system bus")]
    SystemBusAccess,
    /// The given trigger type is not available for the address breakpoint.
    #[error("Unexpected trigger type {0} for address breakpoint.")]
    UnexpectedTriggerType(u32),
    /// The connected target is not a RISC-V device.
    #[error("Connected target is not a RISC-V device.")]
    NoRiscvTarget,
    /// The target does not support halt after reset.
    #[error("The target does not support halt after reset.")]
    ResetHaltRequestNotSupported,
    /// The result index of a batched command is not available.
    #[error("The requested data is not available due to a previous error.")]
    BatchedResultNotAvailable,
    /// The hart is unavailable
    #[error("The requested hart is unavailable.")]
    HartUnavailable,
}

impl From<RiscvError> for ProbeRsError {
    fn from(err: RiscvError) -> Self {
        match err {
            RiscvError::DebugProbe(e) => e.into(),
            other => ProbeRsError::Riscv(other),
        }
    }
}

/// Errors which can occur while executing an abstract command.
#[derive(Debug)]
pub enum AbstractCommandErrorKind {
    /// No error happened.
    None = 0,
    /// An abstract command was executing
    /// while command, `abstractcs`, or `abstractauto`
    /// was written, or when one of the `data` or `progbuf`
    /// registers was read or written. This status is only
    /// written if `cmderr` contains 0.
    Busy = 1,
    /// The requested command is not supported
    NotSupported = 2,
    /// An exception occurred while executing the command (e.g. while executing the Program Buffer).
    Exception = 3,
    /// The abstract command couldn’t
    /// execute because the hart wasn’t in the required
    /// state (running/halted), or unavailable.
    HaltResume = 4,
    /// The abstract command failed due to a
    /// bus error (e.g. alignment, access size, or timeout).
    Bus = 5,
    /// A reserved code. Should not occur.
    _Reserved = 6,
    /// The command failed for another reason.
    Other = 7,
}

impl AbstractCommandErrorKind {
    fn parse(value: u8) -> Self {
        match value {
            0 => Self::None,
            1 => Self::Busy,
            2 => Self::NotSupported,
            3 => Self::Exception,
            4 => Self::HaltResume,
            5 => Self::Bus,
            6 => Self::_Reserved,
            7 => Self::Other,
            _ => unreachable!("cmderr is a 3 bit value, values higher than 7 should not occur."),
        }
    }
}

/// List of all debug module versions.
///
/// The version of the debug module can be read from the version field of the `dmstatus`
/// register.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DebugModuleVersion {
    /// There is no debug module present.
    NoModule,
    /// The debug module conforms to the version 0.11 of the RISC-V Debug Specification.
    Version0_11,
    /// The debug module conforms to the version 0.13 of the RISC-V Debug Specification.
    Version0_13,
    /// The debug module is present, but does not conform to any available version of the RISC-V Debug Specification.
    NonConforming,
    /// Unknown debug module version.
    Unknown(u8),
}

impl From<u8> for DebugModuleVersion {
    fn from(raw: u8) -> Self {
        match raw {
            0 => Self::NoModule,
            1 => Self::Version0_11,
            2 => Self::Version0_13,
            15 => Self::NonConforming,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct CoreRegisterAbstractCmdSupport(u8);

impl CoreRegisterAbstractCmdSupport {
    const READ: Self = Self(1 << 0);
    const WRITE: Self = Self(1 << 1);
    const BOTH: Self = Self(Self::READ.0 | Self::WRITE.0);

    fn supports(&self, o: Self) -> bool {
        self.0 & o.0 == o.0
    }

    fn unset(&mut self, o: Self) {
        self.0 &= !(o.0);
    }
}

/// Save stack of a scratch register.
// TODO: we probably only need an Option, we don't seem to use scratch registers in nested situations.
#[derive(Debug, Default)]
struct ScratchState {
    stack: Vec<u32>,
}

impl ScratchState {
    fn push(&mut self, value: u32) {
        self.stack.push(value);
    }

    fn pop(&mut self) -> Option<u32> {
        self.stack.pop()
    }
}

/// A state to carry all the state data across multiple core switches in a session.
#[derive(Debug)]
pub struct RiscvCommunicationInterfaceState {
    /// Debug specification version
    debug_version: DebugModuleVersion,

    /// Size of the program buffer, in 32-bit words
    progbuf_size: u8,

    /// Cache for the program buffer.
    progbuf_cache: [u32; 16],

    /// Implicit `ebreak` instruction is present after the
    /// the program buffer.
    implicit_ebreak: bool,

    /// Number of data registers for abstract commands
    data_register_count: u8,

    /// Number of scratch registers
    nscratch: u8,

    /// Whether the target supports autoexecuting the program buffer
    supports_autoexec: bool,

    /// Pointer to the configuration string
    confstrptr: Option<u128>,

    /// Width of the hartsel register
    hartsellen: u8,

    /// Number of harts
    num_harts: u32,

    /// Describes, which memory access method should be used for a given access width
    memory_access_info: HashMap<RiscvBusAccess, MemoryAccessMethod>,

    /// describes, if the given register can be read / written with an
    /// abstract command
    abstract_cmd_register_info: HashMap<RegisterId, CoreRegisterAbstractCmdSupport>,

    /// First scratch register's state
    s0: ScratchState,

    /// Second scratch register's state
    s1: ScratchState,

    /// Bitfield of enabled harts
    enabled_harts: u32,

    /// The index of the last selected hart
    last_selected_hart: u32,

    /// Store the value of the `hasresethaltreq` bit of the `dmcstatus` register.
    hasresethaltreq: Option<bool>,
}

/// Timeout for RISC-V operations.
const RISCV_TIMEOUT: Duration = Duration::from_secs(5);

/// RiscV only supports 12bit CSRs. See
/// [Zicsr](https://riscv.org/wp-content/uploads/2019/06/riscv-spec.pdf#chapter.9) extension
const RISCV_MAX_CSR_ADDR: u16 = 0xFFF;

impl RiscvCommunicationInterfaceState {
    /// Create a new interface state.
    pub fn new() -> Self {
        RiscvCommunicationInterfaceState {
            // Set to the minimum here, will be set to the correct value below
            progbuf_size: 0,
            progbuf_cache: [0u32; 16],

            debug_version: DebugModuleVersion::NonConforming,

            // Assume the implicit ebreak is not present
            implicit_ebreak: false,

            // Set to the minimum here, will be set to the correct value below
            data_register_count: 1,

            nscratch: 0,

            supports_autoexec: false,

            confstrptr: None,

            // Assume maximum value, will be determined exactly alter.
            hartsellen: 20,

            // We assume only a singe hart exisits initially
            num_harts: 1,

            memory_access_info: HashMap::new(),

            abstract_cmd_register_info: HashMap::new(),

            s0: ScratchState::default(),
            s1: ScratchState::default(),
            enabled_harts: 0,
            last_selected_hart: 0,
            hasresethaltreq: None,
        }
    }

    /// Get the memory access method which should be used for an
    /// access with the specified width.
    fn memory_access_method(&mut self, access_width: RiscvBusAccess) -> MemoryAccessMethod {
        *self
            .memory_access_info
            .entry(access_width)
            .or_insert(MemoryAccessMethod::ProgramBuffer)
    }
}

impl Default for RiscvCommunicationInterfaceState {
    fn default() -> Self {
        Self::new()
    }
}

/// The combined state of a RISC-V debug module and its transport interface.
pub struct RiscvDebugInterfaceState {
    pub(super) interface_state: RiscvCommunicationInterfaceState,
    pub(super) dtm_state: Box<dyn Any + Send>,
}

impl RiscvDebugInterfaceState {
    pub(super) fn new(dtm_state: Box<dyn Any + Send>) -> Self {
        Self {
            interface_state: RiscvCommunicationInterfaceState::new(),
            dtm_state,
        }
    }
}

/// A single-use factory for creating RISC-V communication interfaces and their states.
pub trait RiscvInterfaceBuilder<'probe> {
    /// Creates a new RISC-V communication interface state object.
    ///
    /// The state object needs to be stored separately from the communication interface
    /// and can be used to restore the state of the interface at a later time.
    fn create_state(&self) -> RiscvDebugInterfaceState;

    /// Consumes the factory and creates a communication interface
    /// object initialised with the given state.
    fn attach<'state>(
        self: Box<Self>,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, DebugProbeError>
    where
        'probe: 'state;
}

/// A interface that implements controls for RISC-V cores.
#[derive(Debug)]
pub struct RiscvCommunicationInterface<'state> {
    /// The Debug Transport Module (DTM) is used to
    /// communicate with the Debug Module on the target chip.
    dtm: Box<dyn DtmAccess + 'state>,
    state: &'state mut RiscvCommunicationInterfaceState,
}

impl<'state> RiscvCommunicationInterface<'state> {
    /// Creates a new RISC-V communication interface with a given probe driver.
    pub fn new(
        dtm: Box<dyn DtmAccess + 'state>,
        state: &'state mut RiscvCommunicationInterfaceState,
    ) -> Self {
        Self { dtm, state }
    }

    /// Select current hart
    pub fn select_hart(&mut self, hart: u32) -> Result<(), RiscvError> {
        if self.state.enabled_harts & (1 << hart) == 0 {
            return Err(RiscvError::HartUnavailable);
        }

        if self.state.last_selected_hart == hart {
            return Ok(());
        }

        let mut control: Dmcontrol = self.read_dm_register()?;
        control.set_dmactive(true);
        control.set_hartsel(hart);
        self.write_dm_register(control)?;
        self.state.last_selected_hart = hart;
        Ok(())
    }

    /// Check if the given hart is enabled
    pub fn hart_enabled(&self, hart: u32) -> bool {
        self.state.enabled_harts & (1 << hart) != 0
    }

    /// Assert the target reset
    pub fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.dtm.target_reset_assert()
    }

    /// Deassert the target reset.
    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.dtm.target_reset_deassert()
    }

    /// Read the targets idcode used as hint for chip detection
    pub fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        self.dtm.read_idcode()
    }

    fn save_s0(&mut self) -> Result<bool, RiscvError> {
        let s0 = self.abstract_cmd_register_read(&registers::S0)?;

        self.state.s0.push(s0);

        Ok(true)
    }

    fn restore_s0(&mut self, saved: bool) -> Result<(), RiscvError> {
        if saved {
            let s0 = self.state.s0.pop().unwrap();

            self.abstract_cmd_register_write(&registers::S0, s0)?;
        }

        Ok(())
    }

    fn save_s1(&mut self) -> Result<bool, RiscvError> {
        let s1 = self.abstract_cmd_register_read(&registers::S1)?;

        self.state.s1.push(s1);

        Ok(true)
    }

    fn restore_s1(&mut self, saved: bool) -> Result<(), RiscvError> {
        if saved {
            let s1 = self.state.s1.pop().unwrap();

            self.abstract_cmd_register_write(&registers::S1, s1)?;
        }

        Ok(())
    }

    pub(crate) fn enter_debug_mode(&mut self) -> Result<(), RiscvError> {
        tracing::debug!("Building RISC-V interface");
        self.dtm.init()?;

        // Reset error bits from previous connections
        self.dtm.clear_error_state()?;

        // enable the debug module
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);
        self.write_dm_register(control)?;

        // read the  version of the debug module
        let status: Dmstatus = self.read_dm_register()?;

        self.state.progbuf_cache.fill(0);
        self.state.debug_version = DebugModuleVersion::from(status.version() as u8);

        // Only version of 0.13 of the debug specification is currently supported.
        if self.state.debug_version != DebugModuleVersion::Version0_13 {
            return Err(RiscvError::UnsupportedDebugModuleVersion(
                self.state.debug_version,
            ));
        }

        self.state.implicit_ebreak = status.impebreak();

        // check if the configuration string pointer is valid, and retrieve it, if valid
        self.state.confstrptr = if status.confstrptrvalid() {
            let confstrptr_0: Confstrptr0 = self.read_dm_register()?;
            let confstrptr_1: Confstrptr1 = self.read_dm_register()?;
            let confstrptr_2: Confstrptr2 = self.read_dm_register()?;
            let confstrptr_3: Confstrptr3 = self.read_dm_register()?;
            let confstrptr = (u32::from(confstrptr_0) as u128)
                | (u32::from(confstrptr_1) as u128) << 8
                | (u32::from(confstrptr_2) as u128) << 16
                | (u32::from(confstrptr_3) as u128) << 32;
            Some(confstrptr)
        } else {
            None
        };

        tracing::debug!("dmstatus: {:?}", status);

        // Select all harts to determine the width
        // of the hartsel register.
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);
        control.set_hartsel(0xffff_ffff);

        self.write_dm_register(control)?;

        let control: Dmcontrol = self.read_dm_register()?;

        self.state.hartsellen = control.hartsel().count_ones() as u8;

        tracing::debug!("HARTSELLEN: {}", self.state.hartsellen);

        // Determine number of harts

        let max_hart_index = 2u32.pow(self.state.hartsellen as u32);

        // Hart 0 exists on every chip
        let mut num_harts = 1;
        self.state.enabled_harts = 1;

        // Check if anynonexistent is avaliable.
        // Some chips that have only one hart do not implement anynonexistent and allnonexistent.
        // So let's check max hart index to see if we can use it reliably,
        // or else we will assume only one hart exists.
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);
        control.set_hartsel(max_hart_index - 1);
        self.write_dm_register(control)?;

        // Check if the anynonexistent works
        let status: Dmstatus = self.read_dm_register()?;

        if status.anynonexistent() {
            for hart_index in 1..max_hart_index {
                let mut control = Dmcontrol(0);
                control.set_dmactive(true);
                control.set_hartsel(hart_index);

                self.write_dm_register(control)?;

                // Check if the current hart exists
                let status: Dmstatus = self.read_dm_register()?;

                if status.anynonexistent() {
                    break;
                }

                if !status.allunavail() {
                    self.state.enabled_harts |= 1 << num_harts;
                }

                num_harts += 1;
            }
        } else {
            tracing::debug!("anynonexistent not supported, assuming only one hart exists")
        }

        tracing::debug!("Number of harts: {}", num_harts);

        self.state.num_harts = num_harts;

        // Select hart 0 again - assuming all harts are same in regards of discovered features
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);
        control.set_hartsel(0);

        self.write_dm_register(control)?;

        // determine size of the program buffer, and number of data
        // registers for abstract commands
        let abstractcs: Abstractcs = self.read_dm_register()?;

        self.state.progbuf_size = abstractcs.progbufsize() as u8;
        tracing::debug!("Program buffer size: {}", self.state.progbuf_size);

        self.state.data_register_count = abstractcs.datacount() as u8;
        tracing::debug!(
            "Number of data registers: {}",
            self.state.data_register_count
        );

        // determine more information about hart
        let hartinfo: Hartinfo = self.read_dm_register()?;

        self.state.nscratch = hartinfo.nscratch() as u8;
        tracing::debug!("Number of dscratch registers: {}", self.state.nscratch);

        // determine if autoexec works
        let mut abstractauto = Abstractauto(0);
        abstractauto.set_autoexecprogbuf(2u32.pow(self.state.progbuf_size as u32) - 1);
        abstractauto.set_autoexecdata(2u32.pow(self.state.data_register_count as u32) - 1);

        self.write_dm_register(abstractauto)?;

        let abstractauto_readback: Abstractauto = self.read_dm_register()?;

        self.state.supports_autoexec = abstractauto_readback == abstractauto;
        tracing::debug!("Support for autoexec: {}", self.state.supports_autoexec);

        // clear abstractauto
        abstractauto = Abstractauto(0);
        self.write_dm_register(abstractauto)?;

        // determine support system bus access
        let sbcs = self.read_dm_register::<Sbcs>()?;

        // Only version 1 is supported, this means that
        // the system bus access conforms to the debug
        // specification 13.2.
        if sbcs.sbversion() == 1 {
            // When possible, we use system bus access for memory access

            if sbcs.sbaccess8() {
                self.state
                    .memory_access_info
                    .insert(RiscvBusAccess::A8, MemoryAccessMethod::SystemBus);
            }

            if sbcs.sbaccess16() {
                self.state
                    .memory_access_info
                    .insert(RiscvBusAccess::A16, MemoryAccessMethod::SystemBus);
            }

            if sbcs.sbaccess32() {
                self.state
                    .memory_access_info
                    .insert(RiscvBusAccess::A32, MemoryAccessMethod::SystemBus);
            }

            if sbcs.sbaccess64() {
                self.state
                    .memory_access_info
                    .insert(RiscvBusAccess::A64, MemoryAccessMethod::SystemBus);
            }

            if sbcs.sbaccess128() {
                self.state
                    .memory_access_info
                    .insert(RiscvBusAccess::A128, MemoryAccessMethod::SystemBus);
            }
        } else {
            tracing::debug!(
                "System bus interface version {} is not supported.",
                sbcs.sbversion()
            );
        }

        Ok(())
    }

    pub(crate) fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, RiscvError> {
        // write 1 to the haltreq register, which is part
        // of the dmcontrol register

        let mut dmcontrol: Dmcontrol = self.read_dm_register()?;
        tracing::debug!(
            "Before requesting halt, the Dmcontrol register value was: {:?}",
            dmcontrol
        );

        dmcontrol.set_dmactive(true);
        dmcontrol.set_haltreq(true);

        self.write_dm_register(dmcontrol)?;

        self.wait_for_core_halted(timeout)?;

        // clear the halt request
        dmcontrol.set_haltreq(false);

        self.write_dm_register(dmcontrol)?;

        let pc: u64 = self
            .read_csr(super::registers::PC.id().0)
            .map(|v| v.into())?;

        Ok(CoreInformation { pc })
    }

    pub(crate) fn core_halted(&mut self) -> Result<bool, RiscvError> {
        let dmstatus: Dmstatus = self.read_dm_register()?;

        tracing::trace!("{:?}", dmstatus);

        Ok(dmstatus.allhalted())
    }

    pub(crate) fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), RiscvError> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while !self.core_halted()? {
            if start.elapsed() >= timeout {
                return Err(RiscvError::Timeout);
            }
            // Wait a bit before polling again.
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    fn halted_access<R>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<R, RiscvError>,
    ) -> Result<R, RiscvError> {
        let was_running = !self.core_halted()?;
        if was_running {
            self.halt(Duration::from_millis(100))?;
        }

        let result = op(self);

        if was_running {
            self.resume_core()?;
        }

        result
    }

    pub(super) fn read_csr(&mut self, address: u16) -> Result<u32, RiscvError> {
        // We need to use the "Access Register Command",
        // which has cmdtype 0

        // write needs to be clear
        // transfer has to be set

        tracing::debug!("Reading CSR {:#x}", address);

        // always try to read register with abstract command, fallback to program buffer,
        // if not supported
        match self.abstract_cmd_register_read(address) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                tracing::debug!("Could not read core register {:#x} with abstract command, falling back to program buffer", address);
                self.read_csr_progbuf(address)
            }
            other => other,
        }
    }

    /// Schedules a DM register read, flushes the queue and returns the result.
    pub(crate) fn read_dm_register<R: MemoryMappedRegister<u32>>(
        &mut self,
    ) -> Result<R, RiscvError> {
        tracing::debug!(
            "Reading DM register '{}' at {:#010x}",
            R::NAME,
            R::get_mmio_address()
        );

        let register_value = self.read_dm_register_untyped(R::get_mmio_address())?.into();

        tracing::debug!(
            "Read DM register '{}' at {:#010x} = {:x?}",
            R::NAME,
            R::get_mmio_address(),
            register_value
        );

        Ok(register_value)
    }

    /// Schedules a DM register read, flushes the queue and returns the untyped result.
    ///
    /// Use the [`Self::read_dm_register()`] function if possible.
    fn read_dm_register_untyped(&mut self, address: u64) -> Result<u32, RiscvError> {
        let read_idx = self.schedule_read_dm_register_untyped(address)?;
        let register_value = self.dtm.read_deferred_result(read_idx)?.into_u32();

        Ok(register_value)
    }

    pub(crate) fn write_dm_register<R: MemoryMappedRegister<u32>>(
        &mut self,
        register: R,
    ) -> Result<(), RiscvError> {
        // write write command to dmi register

        tracing::debug!(
            "Write DM register '{}' at {:#010x} = {:x?}",
            R::NAME,
            R::get_mmio_address(),
            register
        );

        self.write_dm_register_untyped(R::get_mmio_address(), register.into())
    }

    /// Write to a DM register
    ///
    /// Use the [`Self::write_dm_register()`] function if possible.
    fn write_dm_register_untyped(&mut self, address: u64, value: u32) -> Result<(), RiscvError> {
        self.dtm.write_with_timeout(address, value, RISCV_TIMEOUT)?;

        Ok(())
    }

    fn schedule_write_progbuf(&mut self, index: usize, value: u32) -> Result<(), RiscvError> {
        match index {
            0 => self.schedule_write_dm_register(Progbuf0(value)),
            1 => self.schedule_write_dm_register(Progbuf1(value)),
            2 => self.schedule_write_dm_register(Progbuf2(value)),
            3 => self.schedule_write_dm_register(Progbuf3(value)),
            4 => self.schedule_write_dm_register(Progbuf4(value)),
            5 => self.schedule_write_dm_register(Progbuf5(value)),
            6 => self.schedule_write_dm_register(Progbuf6(value)),
            7 => self.schedule_write_dm_register(Progbuf7(value)),
            8 => self.schedule_write_dm_register(Progbuf8(value)),
            9 => self.schedule_write_dm_register(Progbuf9(value)),
            10 => self.schedule_write_dm_register(Progbuf10(value)),
            11 => self.schedule_write_dm_register(Progbuf11(value)),
            12 => self.schedule_write_dm_register(Progbuf12(value)),
            13 => self.schedule_write_dm_register(Progbuf13(value)),
            14 => self.schedule_write_dm_register(Progbuf14(value)),
            15 => self.schedule_write_dm_register(Progbuf15(value)),
            e => Err(RiscvError::UnsupportedProgramBufferRegister(e)),
        }
    }

    pub(crate) fn schedule_setup_program_buffer(&mut self, data: &[u32]) -> Result<(), RiscvError> {
        let required_len = if self.state.implicit_ebreak {
            data.len()
        } else {
            data.len() + 1
        };

        if required_len > self.state.progbuf_size as usize {
            return Err(RiscvError::ProgramBufferTooSmall);
        }

        if data == &self.state.progbuf_cache[..data.len()] {
            // Check if we actually have to write the program buffer
            tracing::debug!("Program buffer is up-to-date, skipping write.");
            return Ok(());
        }

        for (index, word) in data.iter().enumerate() {
            self.schedule_write_progbuf(index, *word)?;
        }

        // Add manual ebreak if necessary.
        //
        // This is necessary when we either don't need the full program buffer,
        // or if there is no implict ebreak after the last program buffer word.
        if !self.state.implicit_ebreak || data.len() < self.state.progbuf_size as usize {
            self.schedule_write_progbuf(data.len(), assembly::EBREAK)?;
        }

        // Update the cache
        self.state.progbuf_cache[..data.len()].copy_from_slice(data);

        Ok(())
    }

    /// Perform a single read from a memory location, using system bus access.
    fn perform_memory_read_sysbus<V: RiscvValue32>(
        &mut self,
        address: u32,
    ) -> Result<V, RiscvError> {
        let mut sbcs = Sbcs(0);

        sbcs.set_sbaccess(V::WIDTH as u32);
        sbcs.set_sbreadonaddr(true);

        self.schedule_write_dm_register(sbcs)?;
        self.schedule_write_dm_register(Sbaddress0(address))?;

        let data_idx = self.schedule_read_large_dtm_register::<V, Sbdata>()?;

        // Check that the read was succesful
        let sbcs = self.read_dm_register::<Sbcs>()?;

        if sbcs.sberror() != 0 {
            Err(RiscvError::SystemBusAccess)
        } else {
            let data = V::from_register_value(self.dtm.read_deferred_result(data_idx)?.into_u32());

            Ok(data)
        }
    }

    /// Perform multiple reads from consecutive memory locations
    /// using system bus access.
    /// Only reads up to a width of 32 bits are currently supported.
    fn perform_memory_read_multiple_sysbus<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: &mut [V],
    ) -> Result<(), RiscvError> {
        let mut sbcs = Sbcs(0);

        sbcs.set_sbaccess(V::WIDTH as u32);

        sbcs.set_sbreadonaddr(true);

        sbcs.set_sbreadondata(true);
        sbcs.set_sbautoincrement(true);

        self.schedule_write_dm_register(sbcs)?;

        self.schedule_write_dm_register(Sbaddress0(address))?;

        let data_len = data.len();

        let mut read_results: Vec<DeferredResultIndex> = vec![];
        for _ in data[..data_len - 1].iter() {
            let idx = self.schedule_read_large_dtm_register::<V, Sbdata>()?;
            read_results.push(idx);
        }

        sbcs.set_sbautoincrement(false);
        self.schedule_write_dm_register(sbcs)?;

        // Read last value
        read_results.push(self.schedule_read_large_dtm_register::<V, Sbdata>()?);

        let sbcs = self.read_dm_register::<Sbcs>()?;

        for (out_index, idx) in read_results.into_iter().enumerate() {
            data[out_index] =
                V::from_register_value(self.dtm.read_deferred_result(idx)?.into_u32());
        }

        // Check that the read was succesful
        if sbcs.sberror() != 0 {
            Err(RiscvError::SystemBusAccess)
        } else {
            Ok(())
        }
    }

    /// Perform memory read from a single location using the program buffer.
    /// Only reads up to a width of 32 bits are currently supported.
    fn perform_memory_read_progbuf<V: RiscvValue32>(
        &mut self,
        address: u32,
    ) -> Result<V, RiscvError> {
        self.halted_access(|core| {
            // assemble
            //  lb s1, 0(s0)

            let s0 = core.save_s0()?;

            let lw_command = assembly::lw(0, 8, V::WIDTH as u8, 8);

            core.schedule_setup_program_buffer(&[lw_command])?;

            core.schedule_write_dm_register(Data0(address))?;

            // Write s0, then execute program buffer
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(true);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            // register s0, ie. 0x1008
            command.set_regno((registers::S0).id.0 as u32);

            core.schedule_write_dm_register(command)?;

            let abstractcs_idx = core.schedule_read_dm_register::<Abstractcs>()?;

            // Read back s0
            let value = core.abstract_cmd_register_read(&registers::S0)?;

            let abstractcs = Abstractcs(core.dtm.read_deferred_result(abstractcs_idx)?.into_u32());
            if abstractcs.cmderr() != 0 {
                return Err(RiscvError::AbstractCommand(
                    AbstractCommandErrorKind::parse(abstractcs.cmderr() as u8),
                ));
            }

            // Restore s0 register
            core.restore_s0(s0)?;

            Ok(V::from_register_value(value))
        })
    }

    fn perform_memory_read_multiple_progbuf<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: &mut [V],
    ) -> Result<(), RiscvError> {
        self.halted_access(|core| {
            // Backup registers s0 and s1
            let s0 = core.save_s0()?;
            let s1 = core.save_s1()?;

            // Load a word from address in register 8 (S0), with offset 0, into register 9 (S9)
            let lw_command: u32 = assembly::lw(0, 8, V::WIDTH as u8, 9);

            core.schedule_setup_program_buffer(&[
                lw_command,
                assembly::addi(8, 8, V::WIDTH.byte_width() as i16),
            ])?;

            core.schedule_write_dm_register(Data0(address))?;

            // Write s0, then execute program buffer
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(true);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            // register s0, ie. 0x1008
            command.set_regno((registers::S0).id.0 as u32);

            core.schedule_write_dm_register(command)?;

            let data_len = data.len();

            let mut result_idxs = Vec::with_capacity(data_len - 1);
            for out_idx in 0..data_len - 1 {
                let mut command = AccessRegisterCommand(0);
                command.set_cmd_type(0);
                command.set_transfer(true);
                command.set_write(false);

                // registers are 32 bit, so we have size 2 here
                command.set_aarsize(RiscvBusAccess::A32);
                command.set_postexec(true);

                command.set_regno((registers::S1).id.0 as u32);

                core.schedule_write_dm_register(command)?;

                // Read back s1
                let value_idx = core.schedule_read_dm_register::<Data0>()?;

                result_idxs.push((out_idx, value_idx));
            }

            // Specifically read the last value first. The result is that this last read is still
            // part of the command queue we just assembled.
            let last_value = core.abstract_cmd_register_read(&registers::S1)?;
            data[data.len() - 1] = V::from_register_value(last_value);

            for (out_idx, value_idx) in result_idxs {
                let value = Data0::from(core.dtm.read_deferred_result(value_idx)?.into_u32());

                data[out_idx] = V::from_register_value(value.0);
            }

            let status: Abstractcs = core.read_dm_register()?;

            if status.cmderr() != 0 {
                return Err(RiscvError::AbstractCommand(
                    AbstractCommandErrorKind::parse(status.cmderr() as u8),
                ));
            }

            core.restore_s0(s0)?;
            core.restore_s1(s1)?;

            Ok(())
        })
    }

    /// Memory write using system bus
    fn perform_memory_write_sysbus<V: RiscvValue>(
        &mut self,
        address: u32,
        data: &[V],
    ) -> Result<(), RiscvError> {
        let mut sbcs = Sbcs(0);

        // Set correct access width
        sbcs.set_sbaccess(V::WIDTH as u32);
        sbcs.set_sbautoincrement(true);

        self.schedule_write_dm_register(sbcs)?;

        self.schedule_write_dm_register(Sbaddress0(address))?;

        for value in data {
            self.schedule_write_large_dtm_register::<V, Sbdata>(*value)?;
        }

        // Check that the write was succesful
        let sbcs = self.read_dm_register::<Sbcs>()?;

        if sbcs.sberror() != 0 {
            Err(RiscvError::SystemBusAccess)
        } else {
            Ok(())
        }
    }

    /// Perform memory write to a single location using the program buffer.
    /// Only writes up to a width of 32 bits are currently supported.
    fn perform_memory_write_progbuf<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: V,
    ) -> Result<(), RiscvError> {
        self.halted_access(|core| {
            tracing::debug!(
                "Memory write using progbuf - {:#010x} = {:#?}",
                address,
                data
            );

            // Backup registers s0 and s1
            let s0 = core.save_s0()?;
            let s1 = core.save_s1()?;

            let sw_command = assembly::sw(0, 8, V::WIDTH as u32, 9);

            core.schedule_setup_program_buffer(&[sw_command])?;

            // write address into s0
            core.abstract_cmd_register_write(&registers::S0, address)?;

            // write data into data 0
            core.schedule_write_dm_register(Data0(data.into()))?;

            // Write s1, then execute program buffer
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(true);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            // register s1, ie. 0x1009
            command.set_regno((registers::S1).id.0 as u32);

            core.schedule_write_dm_register(command)?;

            let status = core.read_dm_register::<Abstractcs>()?;

            if status.cmderr() != 0 {
                let error = AbstractCommandErrorKind::parse(status.cmderr() as u8);

                tracing::error!(
                    "Executing the abstract command for write_{} failed: {:?} ({:x?})",
                    V::WIDTH.byte_width() * 8,
                    error,
                    status,
                );

                return Err(RiscvError::AbstractCommand(error));
            }

            core.restore_s0(s0)?;
            core.restore_s1(s1)?;

            Ok(())
        })
    }

    /// Perform multiple memory writes to consecutive locations using the program buffer.
    /// Only writes up to a width of 32 bits are currently supported.
    fn perform_memory_write_multiple_progbuf<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: &[V],
    ) -> Result<(), RiscvError> {
        self.halted_access(|core| {
            let s0 = core.save_s0()?;
            let s1 = core.save_s1()?;

            // Setup program buffer for multiple writes
            // Store value from register s9 into memory,
            // then increase the address for next write.
            core.schedule_setup_program_buffer(&[
                assembly::sw(0, 8, V::WIDTH as u32, 9),
                assembly::addi(8, 8, V::WIDTH.byte_width() as i16),
            ])?;

            // write address into s0
            core.abstract_cmd_register_write(&registers::S0, address)?;

            for value in data {
                // write address into data 0
                core.schedule_write_dm_register(Data0((*value).into()))?;

                // Write s0, then execute program buffer
                let mut command = AccessRegisterCommand(0);
                command.set_cmd_type(0);
                command.set_transfer(true);
                command.set_write(true);

                // registers are 32 bit, so we have size 2 here
                command.set_aarsize(RiscvBusAccess::A32);
                command.set_postexec(true);

                // register s1
                command.set_regno((registers::S1).id.0 as u32);

                core.schedule_write_dm_register(command)?;
            }

            // Errors are sticky, so we can just check at the end if everything worked.
            let status = core.read_dm_register::<Abstractcs>()?;

            if status.cmderr() != 0 {
                let error = AbstractCommandErrorKind::parse(status.cmderr() as u8);

                tracing::error!(
                    "Executing the abstract command for write_multiple_{} failed: {:?} ({:x?})",
                    V::WIDTH.byte_width() * 8,
                    error,
                    status,
                );

                return Err(RiscvError::AbstractCommand(error));
            }

            // Restore register s0 and s1

            core.restore_s0(s0)?;
            core.restore_s1(s1)?;

            Ok(())
        })
    }

    pub(crate) fn execute_abstract_command(&mut self, command: u32) -> Result<(), RiscvError> {
        // ensure that preconditions are fullfileld
        // haltreq      = 0
        // resumereq    = 0
        // ackhavereset = 0

        let mut dmcontrol: Dmcontrol = self.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_haltreq(false);
        dmcontrol.set_resumereq(false);
        dmcontrol.set_ackhavereset(false);
        self.schedule_write_dm_register(dmcontrol)?;

        // Clear any previous command errors.
        let mut abstractcs_clear = Abstractcs(0);
        abstractcs_clear.set_cmderr(0x7);

        self.schedule_write_dm_register(abstractcs_clear)?;
        self.schedule_write_dm_register(Command(command))?;

        let start_time = Instant::now();

        // Poll busy flag in abstractcs.
        let mut abstractcs;
        loop {
            abstractcs = self.read_dm_register::<Abstractcs>()?;

            if !abstractcs.busy() {
                break;
            }

            if start_time.elapsed() > RISCV_TIMEOUT {
                return Err(RiscvError::Timeout);
            }
        }

        tracing::debug!("abstracts: {:?}", abstractcs);

        // Check command result for error.
        if abstractcs.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::parse(abstractcs.cmderr() as u8),
            ));
        }

        Ok(())
    }

    /// Check if a register can be accessed via abstract commands
    fn check_abstract_cmd_register_support(
        &self,
        regno: RegisterId,
        rw: CoreRegisterAbstractCmdSupport,
    ) -> bool {
        if let Some(status) = self.state.abstract_cmd_register_info.get(&regno) {
            status.supports(rw)
        } else {
            // If not cached yet, assume the register is accessible
            true
        }
    }

    /// Remember, that the given register can not be accessed via abstract commands
    fn set_abstract_cmd_register_unsupported(
        &mut self,
        regno: RegisterId,
        rw: CoreRegisterAbstractCmdSupport,
    ) {
        let entry = self
            .state
            .abstract_cmd_register_info
            .entry(regno)
            .or_insert(CoreRegisterAbstractCmdSupport::BOTH);

        entry.unset(rw);
    }

    // Read a core register using an abstract command
    pub(crate) fn abstract_cmd_register_read(
        &mut self,
        regno: impl Into<RegisterId>,
    ) -> Result<u32, RiscvError> {
        let regno = regno.into();

        // Check if the register was already tried via abstract cmd
        if !self.check_abstract_cmd_register_support(regno, CoreRegisterAbstractCmdSupport::READ) {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::NotSupported,
            ));
        }

        // read from data0
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_aarsize(RiscvBusAccess::A32);

        command.set_regno(regno.0 as u32);

        match self.execute_abstract_command(command.0) {
            Ok(_) => (),
            err @ Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                // Remember, that this register is unsupported
                self.set_abstract_cmd_register_unsupported(
                    regno,
                    CoreRegisterAbstractCmdSupport::READ,
                );
                err?;
            }
            Err(e) => return Err(e),
        }

        let register_value: Data0 = self.read_dm_register()?;

        Ok(register_value.into())
    }

    pub(crate) fn abstract_cmd_register_write<V: RiscvValue>(
        &mut self,
        regno: impl Into<RegisterId>,
        value: V,
    ) -> Result<(), RiscvError> {
        let regno = regno.into();

        // Check if the register was already tried via abstract cmd
        if !self.check_abstract_cmd_register_support(regno, CoreRegisterAbstractCmdSupport::WRITE) {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::NotSupported,
            ));
        }

        // write to data0
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);
        command.set_aarsize(V::WIDTH);

        command.set_regno(regno.0 as u32);

        self.schedule_write_large_dtm_register::<V, Arg0>(value)?;

        match self.execute_abstract_command(command.0) {
            Ok(_) => Ok(()),
            err @ Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                // Remember, that this register is unsupported
                self.set_abstract_cmd_register_unsupported(
                    regno,
                    CoreRegisterAbstractCmdSupport::WRITE,
                );
                err
            }
            Err(e) => Err(e),
        }
    }

    /// Read the CSR `progbuf` register.
    pub fn read_csr_progbuf(&mut self, address: u16) -> Result<u32, RiscvError> {
        self.halted_access(|core| {
            tracing::debug!("Reading CSR {:#04x}", address);

            // Validate that the CSR address is valid
            if address > RISCV_MAX_CSR_ADDR {
                return Err(RiscvError::UnsupportedCsrAddress(address));
            }

            let s0 = core.save_s0()?;

            // Read csr value into register 8 (s0)
            let csrr_cmd = assembly::csrr(8, address);

            core.schedule_setup_program_buffer(&[csrr_cmd])?;

            // command: postexec
            let mut postexec_cmd = AccessRegisterCommand(0);
            postexec_cmd.set_postexec(true);

            core.execute_abstract_command(postexec_cmd.0)?;

            // read the s0 value
            let reg_value = core.abstract_cmd_register_read(&registers::S0)?;

            // restore original value in s0
            core.restore_s0(s0)?;

            Ok(reg_value)
        })
    }

    /// Write the CSR `progbuf` register.
    pub fn write_csr_progbuf(&mut self, address: u16, value: u32) -> Result<(), RiscvError> {
        self.halted_access(|core| {
            tracing::debug!("Writing CSR {:#04x}={}", address, value);

            // Validate that the CSR address is valid
            if address > RISCV_MAX_CSR_ADDR {
                return Err(RiscvError::UnsupportedCsrAddress(address));
            }

            // Backup register s0
            let s0 = core.save_s0()?;

            // Write value into s0
            core.abstract_cmd_register_write(&registers::S0, value)?;

            // Built the CSRW command to write into the program buffer
            let csrw_cmd = assembly::csrw(address, 8);
            core.schedule_setup_program_buffer(&[csrw_cmd])?;

            // command: postexec
            let mut postexec_cmd = AccessRegisterCommand(0);
            postexec_cmd.set_postexec(true);

            core.execute_abstract_command(postexec_cmd.0)?;

            // command: transfer, regno = 0x1008
            // restore original value in s0
            core.restore_s0(s0)?;

            Ok(())
        })
    }

    fn read_word<V: RiscvValue32>(&mut self, address: u32) -> Result<V, crate::Error> {
        let result = match self.state.memory_access_method(V::WIDTH) {
            MemoryAccessMethod::ProgramBuffer => self.perform_memory_read_progbuf(address)?,
            MemoryAccessMethod::SystemBus => self.perform_memory_read_sysbus(address)?,
            MemoryAccessMethod::AbstractCommand => {
                unimplemented!("Memory access using abstract commands is not implemted")
            }
        };

        Ok(result)
    }

    fn read_multiple<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: &mut [V],
    ) -> Result<(), crate::Error> {
        tracing::debug!("read_32 from {:#08x}", address);

        match self.state.memory_access_method(RiscvBusAccess::A32) {
            MemoryAccessMethod::ProgramBuffer => {
                self.perform_memory_read_multiple_progbuf(address, data)?;
            }
            MemoryAccessMethod::SystemBus => {
                self.perform_memory_read_multiple_sysbus(address, data)?;
            }
            MemoryAccessMethod::AbstractCommand => {
                unimplemented!("Memory access using abstract commands is not implemted")
            }
        };

        Ok(())
    }

    fn write_word<V: RiscvValue32>(&mut self, address: u32, data: V) -> Result<(), crate::Error> {
        match self.state.memory_access_method(V::WIDTH) {
            MemoryAccessMethod::ProgramBuffer => {
                self.perform_memory_write_progbuf(address, data)?
            }
            MemoryAccessMethod::SystemBus => self.perform_memory_write_sysbus(address, &[data])?,
            MemoryAccessMethod::AbstractCommand => {
                unimplemented!("Memory access using abstract commands is not implemted")
            }
        };

        Ok(())
    }

    fn write_multiple<V: RiscvValue32>(
        &mut self,
        address: u32,
        data: &[V],
    ) -> Result<(), crate::Error> {
        match self.state.memory_access_method(V::WIDTH) {
            MemoryAccessMethod::SystemBus => self.perform_memory_write_sysbus(address, data)?,
            MemoryAccessMethod::ProgramBuffer => {
                self.perform_memory_write_multiple_progbuf(address, data)?
            }
            MemoryAccessMethod::AbstractCommand => {
                unimplemented!("Memory access using abstract commands is not implemted")
            }
        }

        Ok(())
    }

    pub(crate) fn execute(&mut self) -> Result<(), RiscvError> {
        self.dtm.execute()
    }

    pub(crate) fn schedule_write_dm_register<R: MemoryMappedRegister<u32>>(
        &mut self,
        register: R,
    ) -> Result<(), RiscvError> {
        // write write command to dmi register

        tracing::debug!(
            "Write DM register '{}' at {:#010x} = {:x?}",
            R::NAME,
            R::get_mmio_address(),
            register
        );

        self.schedule_write_dm_register_untyped(R::get_mmio_address(), register.into())?;
        Ok(())
    }

    /// Write to a DM register
    ///
    /// Use the [`Self::schedule_write_dm_register()`] function if possible.
    fn schedule_write_dm_register_untyped(
        &mut self,
        address: u64,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError> {
        self.dtm.schedule_write(address, value)
    }

    pub(super) fn schedule_read_dm_register<R: MemoryMappedRegister<u32>>(
        &mut self,
    ) -> Result<DeferredResultIndex, RiscvError> {
        tracing::debug!(
            "Reading DM register '{}' at {:#010x}",
            R::NAME,
            R::get_mmio_address()
        );

        self.schedule_read_dm_register_untyped(R::get_mmio_address())
    }

    /// Read from a DM register
    ///
    /// Use the [`Self::schedule_read_dm_register()`] function if possible.
    fn schedule_read_dm_register_untyped(
        &mut self,
        address: u64,
    ) -> Result<DeferredResultIndex, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.dtm.schedule_read(address)
    }

    fn schedule_read_large_dtm_register<V, R>(&mut self) -> Result<DeferredResultIndex, RiscvError>
    where
        V: RiscvValue,
        R: LargeRegister,
    {
        V::schedule_read_from_register::<R>(self)
    }

    fn schedule_write_large_dtm_register<V, R>(
        &mut self,
        value: V,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        V: RiscvValue,
        R: LargeRegister,
    {
        V::schedule_write_to_register::<R>(self, value)
    }

    /// Check if the connected device supports halt after reset.
    ///
    /// Returns a cached value if available, otherwise queries the
    /// `hasresethaltreq` bit in the `dmstatus` register.
    pub(crate) fn supports_reset_halt_req(&mut self) -> Result<bool, RiscvError> {
        if let Some(has_reset_halt_req) = self.state.hasresethaltreq {
            Ok(has_reset_halt_req)
        } else {
            let dmstatus: Dmstatus = self.read_dm_register()?;

            self.state.hasresethaltreq = Some(dmstatus.hasresethaltreq());

            Ok(dmstatus.hasresethaltreq())
        }
    }

    // Resume the core.
    pub(crate) fn resume_core(&mut self) -> Result<(), RiscvError> {
        // set resume request.
        let mut dmcontrol: Dmcontrol = self.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        self.write_dm_register(dmcontrol)?;

        // check if request has been acknowleged.
        let status: Dmstatus = self.read_dm_register()?;
        if !status.allresumeack() {
            return Err(RiscvError::RequestNotAcknowledged);
        }

        // clear resume request.
        dmcontrol.set_resumereq(false);
        self.write_dm_register(dmcontrol)?;

        Ok(())
    }

    pub(crate) fn reset_hart_and_halt(&mut self, timeout: Duration) -> Result<(), RiscvError> {
        tracing::debug!("Resetting core, setting hartreset bit");

        let mut dmcontrol: Dmcontrol = self.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_hartreset(true);
        dmcontrol.set_haltreq(true);

        self.write_dm_register(dmcontrol)?;

        // Read back register to verify reset is supported
        let readback: Dmcontrol = self.read_dm_register()?;

        if readback.hartreset() {
            tracing::debug!("Clearing hartreset bit");
            // Reset is performed by setting the bit high, and then low again
            let mut dmcontrol = readback;
            dmcontrol.set_dmactive(true);
            dmcontrol.set_hartreset(false);

            self.write_dm_register(dmcontrol)?;
        } else {
            // Hartreset is not supported, whole core needs to be reset
            //
            // TODO: Cache this
            tracing::debug!("Hartreset bit not supported, using ndmreset");
            dmcontrol.set_hartreset(false);
            dmcontrol.set_ndmreset(true);
            dmcontrol.set_haltreq(true);

            self.write_dm_register(dmcontrol)?;

            tracing::debug!("Clearing ndmreset bit");
            dmcontrol.set_ndmreset(false);
            dmcontrol.set_haltreq(true);

            self.write_dm_register(dmcontrol)?;
        }

        let start = Instant::now();

        loop {
            // check that cores have reset
            let readback: Dmstatus = self.read_dm_register()?;

            if readback.allhavereset() && readback.allhalted() {
                break;
            }

            if start.elapsed() > timeout {
                return Err(RiscvError::RequestNotAcknowledged);
            }
        }

        // clear the reset request
        dmcontrol.set_haltreq(false);
        dmcontrol.set_ackhavereset(true);
        dmcontrol.set_hartreset(false);
        dmcontrol.set_ndmreset(false);

        self.write_dm_register(dmcontrol)?;

        // Reenable halt on breakpoint because this gets disabled if we reset the core
        self.debug_on_sw_breakpoint(true)?; // TODO: only restore if enabled before?

        Ok(())
    }

    pub(crate) fn debug_on_sw_breakpoint(&mut self, enabled: bool) -> Result<(), RiscvError> {
        let mut dcsr = Dcsr(self.read_csr(0x7b0)?);

        dcsr.set_ebreakm(enabled);
        dcsr.set_ebreaks(enabled);
        dcsr.set_ebreaku(enabled);

        match self.abstract_cmd_register_write(0x7b0, dcsr.0) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                tracing::debug!("Could not write core register {:#x} with abstract command, falling back to program buffer", 0x7b0);
                self.write_csr_progbuf(0x7b0, dcsr.0)
            }
            other => other,
        }
    }
}

pub(crate) trait LargeRegister {
    const R0_ADDRESS: u8;
    const R1_ADDRESS: u8;
    const R2_ADDRESS: u8;
    const R3_ADDRESS: u8;
}

struct Sbdata {}

impl LargeRegister for Sbdata {
    const R0_ADDRESS: u8 = Sbdata0::ADDRESS_OFFSET as u8;
    const R1_ADDRESS: u8 = Sbdata1::ADDRESS_OFFSET as u8;
    const R2_ADDRESS: u8 = Sbdata2::ADDRESS_OFFSET as u8;
    const R3_ADDRESS: u8 = Sbdata3::ADDRESS_OFFSET as u8;
}

struct Arg0 {}

impl LargeRegister for Arg0 {
    const R0_ADDRESS: u8 = Data0::ADDRESS_OFFSET as u8;
    const R1_ADDRESS: u8 = Data1::ADDRESS_OFFSET as u8;
    const R2_ADDRESS: u8 = Data2::ADDRESS_OFFSET as u8;
    const R3_ADDRESS: u8 = Data3::ADDRESS_OFFSET as u8;
}

/// Helper trait, limited to RiscvValue no larger than 32 bits
pub(crate) trait RiscvValue32: RiscvValue + Into<u32> {
    fn from_register_value(value: u32) -> Self;
}

impl RiscvValue32 for u8 {
    fn from_register_value(value: u32) -> Self {
        value as u8
    }
}
impl RiscvValue32 for u16 {
    fn from_register_value(value: u32) -> Self {
        value as u16
    }
}
impl RiscvValue32 for u32 {
    fn from_register_value(value: u32) -> Self {
        value
    }
}

/// Marker trait for different values which
/// can be read / written using the debug module.
pub(crate) trait RiscvValue: std::fmt::Debug + Copy + Sized {
    const WIDTH: RiscvBusAccess;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister;

    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister;
}

impl RiscvValue for u8 {
    const WIDTH: RiscvBusAccess = RiscvBusAccess::A8;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_read_dm_register_untyped(R::R0_ADDRESS as u64)
    }

    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_write_dm_register_untyped(R::R0_ADDRESS as u64, value as u32)
    }
}

impl RiscvValue for u16 {
    const WIDTH: RiscvBusAccess = RiscvBusAccess::A16;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_read_dm_register_untyped(R::R0_ADDRESS as u64)
    }

    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_write_dm_register_untyped(R::R0_ADDRESS as u64, value as u32)
    }
}

impl RiscvValue for u32 {
    const WIDTH: RiscvBusAccess = RiscvBusAccess::A32;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_read_dm_register_untyped(R::R0_ADDRESS as u64)
    }
    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_write_dm_register_untyped(R::R0_ADDRESS as u64, value)
    }
}

impl RiscvValue for u64 {
    const WIDTH: RiscvBusAccess = RiscvBusAccess::A64;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_read_dm_register_untyped(R::R1_ADDRESS as u64)?;
        interface.schedule_read_dm_register_untyped(R::R0_ADDRESS as u64)
    }

    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister,
    {
        let upper_bits = (value >> 32) as u32;
        let lower_bits = (value & 0xffff_ffff) as u32;

        // R0 has to be written last, side effects are triggerd by writes from
        // this register.

        interface.schedule_write_dm_register_untyped(R::R1_ADDRESS as u64, upper_bits)?;
        interface.schedule_write_dm_register_untyped(R::R0_ADDRESS as u64, lower_bits)
    }
}

impl RiscvValue for u128 {
    const WIDTH: RiscvBusAccess = RiscvBusAccess::A128;

    fn schedule_read_from_register<R>(
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<DeferredResultIndex, RiscvError>
    where
        R: LargeRegister,
    {
        interface.schedule_read_dm_register_untyped(R::R3_ADDRESS as u64)?;
        interface.schedule_read_dm_register_untyped(R::R2_ADDRESS as u64)?;
        interface.schedule_read_dm_register_untyped(R::R1_ADDRESS as u64)?;
        interface.schedule_read_dm_register_untyped(R::R0_ADDRESS as u64)
    }

    fn schedule_write_to_register<R>(
        interface: &mut RiscvCommunicationInterface,
        value: Self,
    ) -> Result<Option<DeferredResultIndex>, RiscvError>
    where
        R: LargeRegister,
    {
        let bits_3 = (value >> 96) as u32;
        let bits_2 = (value >> 64) as u32;
        let bits_1 = (value >> 32) as u32;
        let bits_0 = (value & 0xffff_ffff) as u32;

        // R0 has to be written last, side effects are triggerd by writes from
        // this register.

        interface.schedule_write_dm_register_untyped(R::R3_ADDRESS as u64, bits_3)?;
        interface.schedule_write_dm_register_untyped(R::R2_ADDRESS as u64, bits_2)?;
        interface.schedule_write_dm_register_untyped(R::R1_ADDRESS as u64, bits_1)?;
        interface.schedule_write_dm_register_untyped(R::R0_ADDRESS as u64, bits_0)
    }
}

impl MemoryInterface for RiscvCommunicationInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, crate::error::Error> {
        let address = valid_32bit_address(address)?;
        let mut ret = self.read_word::<u32>(address)? as u64;
        ret |= (self.read_word::<u32>(address + 4)? as u64) << 32;

        Ok(ret)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_word_32 from {:#08x}", address);
        self.read_word(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_word_16 from {:#08x}", address);
        self.read_word(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_word_8 from {:#08x}", address);
        self.read_word(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::error::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_64 from {:#08x}", address);

        for (i, d) in data.iter_mut().enumerate() {
            *d = self.read_word_64((address + (i as u32 * 8)).into())?;
        }

        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_32 from {:#08x}", address);
        self.read_multiple(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_16 from {:#08x}", address);
        self.read_multiple(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("read_8 from {:#08x}", address);

        self.read_multiple(address, data)
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        self.read_multiple(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), crate::error::Error> {
        let address = valid_32bit_address(address)?;
        let low_word = data as u32;
        let high_word = (data >> 32) as u32;

        self.write_word(address, low_word)?;
        self.write_word(address + 4, high_word)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        self.write_word(address, data)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        self.write_word(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        self.write_word(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::error::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("write_64 to {:#08x}", address);

        for (i, d) in data.iter().enumerate() {
            self.write_word_64((address + (i as u32 * 8)).into(), *d)?;
        }

        Ok(())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("write_32 to {:#08x}", address);

        self.write_multiple(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("write_16 to {:#08x}", address);

        self.write_multiple(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        tracing::debug!("write_8 to {:#08x}", address);

        self.write_multiple(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        let address = valid_32bit_address(address)?;
        self.write_multiple(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Access width for bus access.
/// This is used both for system bus access (`sbcs` register),
/// as well for abstract commands.
#[derive(Copy, Clone, PartialEq, PartialOrd, Hash, Eq, Debug)]
pub enum RiscvBusAccess {
    /// 1 byte
    A8 = 0,
    /// 2 bytes
    A16 = 1,
    /// 4 bytes
    A32 = 2,
    /// 8 bytes
    A64 = 3,
    /// 16 bytes.
    A128 = 4,
}

impl RiscvBusAccess {
    /// Width of an access in bytes
    const fn byte_width(&self) -> usize {
        match self {
            RiscvBusAccess::A8 => 1,
            RiscvBusAccess::A16 => 2,
            RiscvBusAccess::A32 => 4,
            RiscvBusAccess::A64 => 8,
            RiscvBusAccess::A128 => 16,
        }
    }
}

impl From<RiscvBusAccess> for u8 {
    fn from(value: RiscvBusAccess) -> Self {
        value as u8
    }
}

/// Different methods of memory access,
/// which can be supported by a debug module.
///
/// The `AbstractCommand` method for memory access is not implemented.
#[derive(Debug, Copy, Clone)]
#[allow(dead_code)]
enum MemoryAccessMethod {
    /// Memory access using the program buffer is supported
    ProgramBuffer,
    /// Memory access using an abstract command is supported
    AbstractCommand,
    /// Memory access using system bus access supported
    SystemBus,
}

memory_mapped_bitfield_register! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec.
    pub struct AccessRegisterCommand(u32);
    0x17, "command",
    impl From;
    /// This is 0 to indicate Access Register Command.
    pub _, set_cmd_type: 31, 24;
    /// 2: Access the lowest 32 bits of the register.\
    /// 3: Access the lowest 64 bits of the register.\
    /// 4: Access the lowest 128 bits of the register.
    ///
    /// If `aarsize` specifies a size larger than the register’s
    /// actual size, then the access must fail. If a register is accessible, then reads of `aarsize` less than
    /// or equal to the register’s actual size must be supported.
    ///
    /// This field controls the Argument Width as referenced in Table 3.1.
    pub u8, from into RiscvBusAccess, _, set_aarsize: 22, 20;
    /// 0: No effect. This variant must be supported.\
    /// 1: After a successful register access, `regno` is incremented (wrapping around to 0). Supporting
    /// this variant is optional.
    pub _, set_aarpostincrement: 19;
    /// 0: No effect. This variant must be supported, and
    /// is the only supported one if `progbufsize` is 0.\
    /// 1: Execute the program in the Program Buffer
    /// exactly once after performing the transfer, if any.
    /// Supporting this variant is optional.
    pub _, set_postexec: 18;
    /// 0: Don’t do the operation specified by write.\
    /// 1: Do the operation specified by write.
    /// This bit can be used to just execute the Program Buffer without having to worry about placing valid values into `aarsize` or `regno`
    pub _, set_transfer: 17;
    /// When transfer is set: 0: Copy data from the specified register into arg0 portion of data.
    /// 1: Copy data from arg0 portion of data into the
    /// specified register.
    pub _, set_write: 16;
    /// Number of the register to access, as described in
    /// Table 3.3. dpc may be used as an alias for PC if
    /// this command is supported on a non-halted hart.
    pub _, set_regno: 15, 0;
}

memory_mapped_bitfield_register! {
    /// System Bus Access Control and Status (see 3.12.18)
    pub struct Sbcs(u32);
    0x38, "sbcs",
    impl From;
    /// 0: The System Bus interface conforms to mainline
    /// drafts of this spec older than 1 January, 2018.\
    /// 1: The System Bus interface conforms to this version of the spec.
    ///
    /// Other values are reserved for future versions
    sbversion, _: 31, 29;
    /// Set when the debugger attempts to read data
    /// while a read is in progress, or when the debugger initiates a new access while one is already in
    /// progress (while `sbbusy` is set). It remains set until
    /// it’s explicitly cleared by the debugger.
    /// While this field is set, no more system bus accesses
    /// can be initiated by the Debug Module.
    sbbusyerror, set_sbbusyerror: 22;
    /// When 1, indicates the system bus master is busy.
    /// (Whether the system bus itself is busy is related,
    /// but not the same thing.) This bit goes high immediately when a read or write is requested for
    /// any reason, and does not go low until the access
    /// is fully completed.
    ///
    /// Writes to `sbcs` while `sbbusy` is high result in undefined behavior. A debugger must not write to
    /// sbcs until it reads `sbbusy` as 0.
    sbbusy, _: 21;
    /// When 1, every write to `sbaddress0` automatically
    /// triggers a system bus read at the new address.
    sbreadonaddr, set_sbreadonaddr: 20;
    /// Select the access size to use for system bus accesses.
    ///
    /// 0: 8-bit\
    /// 1: 16-bit\
    /// 2: 32-bit\
    /// 3: 64-bit\
    /// 4: 128-bit
    ///
    /// If `sbaccess` has an unsupported value when the
    /// DM starts a bus access, the access is not performed and `sberror` is set to 4.
    sbaccess, set_sbaccess: 19, 17;
    /// When 1, `sbaddress` is incremented by the access
    /// size (in bytes) selected in `sbaccess` after every system bus access.
    sbautoincrement, set_sbautoincrement: 16;
    /// When 1, every read from `sbdata0` automatically
    /// triggers a system bus read at the (possibly autoincremented) address.
    sbreadondata, set_sbreadondata: 15;
    /// When the Debug Module’s system bus master encounters an error, this field gets set. The bits in
    /// this field remain set until they are cleared by writing 1 to them. While this field is non-zero, no
    /// more system bus accesses can be initiated by the
    /// Debug Module.
    /// An implementation may report “Other” (7) for any error condition.
    ///
    /// 0: There was no bus error.\
    /// 1: There was a timeout.\
    /// 2: A bad address was accessed.\
    /// 3: There was an alignment error.\
    /// 4: An access of unsupported size was requested.\
    /// 7: Other.
    sberror, set_sberror: 14, 12;
    /// Width of system bus addresses in bits. (0 indicates there is no bus access support.)
    sbasize, _: 11, 5;
    /// 1 when 128-bit system bus accesses are supported.
    sbaccess128, _: 4;
    /// 1 when 64-bit system bus accesses are supported.
    sbaccess64, _: 3;
    /// 1 when 32-bit system bus accesses are supported.
    sbaccess32, _: 2;
    /// 1 when 16-bit system bus accesses are supported.
    sbaccess16, _: 1;
    /// 1 when 8-bit system bus accesses are supported.
    sbaccess8, _: 0;
}

memory_mapped_bitfield_register! {
    /// Abstract Command Autoexec (see 3.12.8)
    #[derive(Eq, PartialEq)]
    pub struct Abstractauto(u32);
    0x18, "abstractauto",
    impl From;
    /// When a bit in this field is 1, read or write accesses to the corresponding `progbuf` word cause
    /// the command in command to be executed again.
    autoexecprogbuf, set_autoexecprogbuf: 31, 16;
    /// When a bit in this field is 1, read or write accesses to the corresponding data word cause the
    /// command in command to be executed again.
    autoexecdata, set_autoexecdata: 11, 0;
}

memory_mapped_bitfield_register! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec. (see 3.6.1.3)
    pub struct AccessMemoryCommand(u32);
    0x17, "command",
    /// This is 2 to indicate Access Memory Command.
    _, set_cmd_type: 31, 24;
    /// An implementation does not have to implement
    /// both virtual and physical accesses, but it must
    /// fail accesses that it doesn’t support.

    /// 0: Addresses are physical (to the hart they are
    /// performed on).\
    /// 1: Addresses are virtual, and translated the way
    /// they would be from M-mode, with `MPRV` set.
    pub _, set_aamvirtual: 23;
    /// 0: Access the lowest 8 bits of the memory location.\
    /// 1: Access the lowest 16 bits of the memory location.\
    /// 2: Access the lowest 32 bits of the memory location.\
    /// 3: Access the lowest 64 bits of the memory location.\
    /// 4: Access the lowest 128 bits of the memory location.
    pub _, set_aamsize: 22,20;
    /// After a memory access has completed, if this bit
    /// is 1, increment arg1 (which contains the address
    /// used) by the number of bytes encoded in `aamsize`.
    pub _, set_aampostincrement: 19;
    /// 0: Copy data from the memory location specified
    /// in arg1 into arg0 portion of data.\
    /// 1: Copy data from arg0 portion of data into the
    /// memory location specified in arg1.
    pub _, set_write: 16;
    /// These bits are reserved for target-specific uses.
    pub _, set_target_specific: 15, 14;
}

impl From<AccessMemoryCommand> for u32 {
    fn from(register: AccessMemoryCommand) -> Self {
        let mut reg = register;
        reg.set_cmd_type(2);
        reg.0
    }
}

impl From<u32> for AccessMemoryCommand {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

memory_mapped_bitfield_register! { pub struct Sbaddress0(u32); 0x39, "sbaddress0", impl From; }
memory_mapped_bitfield_register! { pub struct Sbaddress1(u32); 0x3a, "sbaddress1", impl From; }
memory_mapped_bitfield_register! { pub struct Sbaddress2(u32); 0x3b, "sbaddress2", impl From; }
memory_mapped_bitfield_register! { pub struct Sbaddress3(u32); 0x37, "sbaddress3", impl From; }

memory_mapped_bitfield_register! { pub struct Sbdata0(u32); 0x3c, "sbdata0", impl From; }
memory_mapped_bitfield_register! { pub struct Sbdata1(u32); 0x3d, "sbdata1", impl From; }
memory_mapped_bitfield_register! { pub struct Sbdata2(u32); 0x3e, "sbdata2", impl From; }
memory_mapped_bitfield_register! { pub struct Sbdata3(u32); 0x3f, "sbdata3", impl From; }

memory_mapped_bitfield_register! { pub struct Confstrptr0(u32); 0x19, "confstrptr0", impl From; }
memory_mapped_bitfield_register! { pub struct Confstrptr1(u32); 0x1a, "confstrptr1", impl From; }
memory_mapped_bitfield_register! { pub struct Confstrptr2(u32); 0x1b, "confstrptr2", impl From; }
memory_mapped_bitfield_register! { pub struct Confstrptr3(u32); 0x1c, "confstrptr3", impl From; }
