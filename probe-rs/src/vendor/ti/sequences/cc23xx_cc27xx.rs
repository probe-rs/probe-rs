//! Sequences for cc23xx_cc27xx devices
use bitfield::bitfield;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::MemoryMappedRegister;
use crate::architecture::arm::ArmProbeInterface;
use crate::architecture::arm::DapAccess;
use crate::architecture::arm::armv6m::{Aircr, BpCtrl, Demcr, Dhcsr};
use crate::architecture::arm::core::cortex_m;
use crate::architecture::arm::dp::{Abort, Ctrl, DebugPortError, DpAccess, DpAddress, SelectV1};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, cortex_m_core_start};
use crate::architecture::arm::{ArmError, FullyQualifiedApAddress};
use probe_rs_target::CoreType;

/// Marker struct indicating initialization sequencing for cc23xx_cc27xx family parts.
#[derive(Debug)]
pub struct CC23xxCC27xx {
    /// Chip name - this will be used when more targets are added
    _name: String,
    /// Flag to indicate if the ROM is in the boot loop
    boot_loop: AtomicBool,
}

/// Enum representing the Access Port Select register values
#[derive(Debug, Clone, Copy)]
enum ApSel {
    /// Config-AP: This is the AP used to read device type information
    CfgAp = 1,
    /// Sec-AP: This is the AP used to send SACI commands
    SecAp = 2,
}

bitfield! {
    /// Device Status Register, part of CFG-AP.
    ///
    /// This register is used to read the device status and boot status.
    #[derive(Copy, Clone)]
    pub struct DeviceStatusRegister(u32);
    impl Debug;
    ///  Bit describing if the AHB-AP is available
    ///
    /// `0`: Device is in SACI mode\
    /// `1`: Device is not in SACI mode and AHB-AP is available
    pub ahb_ap_available, _: 24;

    /// Boot Status
    ///
    /// This field is used to read the boot status of the device.
    pub u8, boot_status, _: 15, 8;
}

impl DeviceStatusRegister {
    /// Address of the device status register within the CFG-AP.
    pub const DEVICE_STATUS_REGISTER_ADDRESS: u64 = 0x0C;

    /// Read the device status register from the CFG-AP.
    pub fn read(interface: &mut dyn DapAccess) -> Result<Self, ArmError> {
        let cfg_ap: FullyQualifiedApAddress = ApSel::CfgAp.into();
        let contents =
            interface.read_raw_ap_register(&cfg_ap, Self::DEVICE_STATUS_REGISTER_ADDRESS)?;
        Ok(Self(contents))
    }
}

const BOOT_STATUS_APP_WAITLOOP_DBGPROBE: u8 = 0xC1;
const BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE: u8 = 0x81;
const BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE: u8 = 0x38;

bitfield! {
    /// TX_CTRL Register, part of SEC-AP.
    ///
    /// This register is used to control the transmission of SACI commands.
    #[derive(Copy, Clone)]
    pub struct TxCtrlRegister(u32);
    impl Debug;
    /// Bit indicating if the TXD register is ready.
    ///
    /// Indicates that TXD can be read. Set by hardware when TXD is written, cleared by hardware when TXD is read
    ///
    /// `0`: TXD is ready
    /// `1`: TXD is not ready
    pub txd_full, _: 0;
    /// Command Start
    ///
    /// This field is used to start a command.
    pub cmd_start, set_cmd_start: 1;
}

impl TxCtrlRegister {
    /// Address of the TX_CTRL register within the SEC-AP.
    pub const TX_CTRL_REGISTER_ADDRESS: u64 = 4;

    /// Read the TX_CTRL register from the SEC-AP.
    pub fn read(interface: &mut dyn DapAccess) -> Result<Self, ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
        let contents = interface.read_raw_ap_register(&sec_ap, Self::TX_CTRL_REGISTER_ADDRESS)?;
        Ok(Self(contents))
    }

    /// Write the TX_CTRL register to the SEC-AP.
    pub fn write(&self, interface: &mut dyn DapAccess) -> Result<(), ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
        interface.write_raw_ap_register(&sec_ap, Self::TX_CTRL_REGISTER_ADDRESS, self.0)
    }
}

impl From<ApSel> for FullyQualifiedApAddress {
    fn from(apsel: ApSel) -> Self {
        FullyQualifiedApAddress::v1_with_default_dp(apsel as u8)
    }
}

impl CC23xxCC27xx {
    /// Create the sequencer for the cc23xx_cc27xx family of parts.
    pub fn create(name: String) -> Arc<Self> {
        Arc::new(Self {
            _name: name,
            boot_loop: AtomicBool::new(false),
        })
    }

    /// Check if the ROM is in the boot loop
    ///
    /// The boot loop is a state where the ROM is waiting for a debugger to attach and write to R3 to exit the loop.
    /// This needs to be tracked across multiple debug sequence states so it is stored on the host.
    fn is_in_boot_loop(&self) -> bool {
        self.boot_loop.load(Ordering::SeqCst)
    }

    /// Polls the TX_CTRL register until it is ready or a timeout occurs.
    ///
    /// This function reads the TX_CTRL register in a loop until it indicates readiness
    /// or the specified timeout duration has elapsed.
    ///
    /// # Arguments
    ///
    /// * `interface` - A mutable reference to the ARM communication interface.
    /// * `timeout` - The maximum duration to wait for the TX_CTRL register to be ready.
    ///
    /// # Returns
    ///
    /// * `Result<(), ArmError>` - Returns `Ok(())` if the TX_CTRL register is ready,
    ///   or an `ArmError` if there was a timeout.
    fn poll_tx_ctrl(
        &self,
        interface: &mut dyn DapAccess,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        let mut tx_ctrl = TxCtrlRegister::read(interface)?;
        TxCtrlRegister::read(interface)?;
        while tx_ctrl.txd_full() {
            if start.elapsed() >= timeout {
                return Err(ArmError::Timeout);
            }
            tx_ctrl = TxCtrlRegister::read(interface)?;
        }
        Ok(())
    }

    /// Sends a SACI command to the device.
    ///
    /// This function communicates with the device using the Security Access Port (SEC AP)
    /// to send a SACI command. It waits for the TX_CTRL register to be ready before sending
    /// the command and then writes the command to the TX_DATA register. Again waiting for TX_CTRL to be ready.
    ///
    /// Implements Section 8.3.1.1 from https://www.ti.com/lit/ug/swcu193/swcu193.pdf
    ///
    /// # Arguments
    ///
    /// * `interface` - A mutable reference to the ARM communication interface.
    /// * `command` - The SACI command to be sent.
    ///
    /// # Returns
    ///
    /// * `Result<(), ArmError>` - Returns `Ok(())` if the command was successfully sent,
    ///   or an `ArmError` if there was an error during communication.
    ///
    fn saci_command(&self, interface: &mut dyn DapAccess, command: u32) -> Result<(), ArmError> {
        let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();

        const TX_DATA_ADDR: u64 = 0;

        // Wait for tx_ctrl to be ready with a timeout of 1 millisecond
        self.poll_tx_ctrl(interface, Duration::from_millis(1))?;

        // Set Cmd Start
        let mut tx_ctrl = TxCtrlRegister(0);
        tx_ctrl.set_cmd_start(true);
        TxCtrlRegister::write(&tx_ctrl, interface)?;

        // Write parameter word to txd
        interface.write_raw_ap_register(&sec_ap, TX_DATA_ADDR, command)?;

        self.poll_tx_ctrl(interface, Duration::from_millis(1))?;

        Ok(())
    }
}

impl ArmDebugSequence for CC23xxCC27xx {
    fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Check if the previous code requested a halt before reset
        let demcr = Demcr(probe.read_word_32(Demcr::get_mmio_address())?);

        // Read if breakpoints should be enabled after reset
        let mut bpt_ctrl = BpCtrl(probe.read_word_32(BpCtrl::get_mmio_address())?);

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        // Reset the device, flush all pending writes and wait on the reset to complete
        probe.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        probe.flush().ok();
        thread::sleep(Duration::from_millis(10));

        // Re-initializing the core(s) is on us.
        let ap = probe.fully_qualified_address();
        let interface = probe.get_arm_probe_interface()?;

        interface.reinitialize()?;
        self.debug_core_start(interface, &ap, core_type, debug_base, None)?;

        // Halt the CPU
        if demcr.vc_corereset() {
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();

            probe.write_word_32(Dhcsr::get_mmio_address(), value.into())?;
        }

        // Restore the breakpoint control register
        bpt_ctrl.set_key(true);
        probe.write_word_32(BpCtrl::get_mmio_address(), bpt_ctrl.into())?;

        Ok(())
    }

    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // TODO:
        // Copy-pasted from the default Trait implementation, but we need to add
        // the cc23xx_cc27xx specific parts at the end
        // This code is from `debug_port_start` in `probe-rs/src/architecture/arm/sequences.rs`

        let mut abort = Abort(0);
        abort.set_dapabort(true);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        interface.write_dp_register(dp, abort)?;

        interface.write_dp_register(dp, SelectV1(0))?;

        let ctrl = interface.read_dp_register::<Ctrl>(dp)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            tracing::info!("Debug port {dp:x?} is powered down, powering up");
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            interface.write_dp_register(dp, ctrl)?;

            let start = Instant::now();
            loop {
                let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
            }

            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            ctrl.set_mask_lane(0b1111);
            interface.write_dp_register(dp, ctrl)?;

            let ctrl_reg: Ctrl = interface.read_dp_register(dp)?;
            if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
                tracing::error!("Debug power request failed");
                return Err(DebugPortError::TargetPowerUpFailed.into());
            }

            // According to CMSIS docs, here's where we would clear errors
            // in ABORT, but we do that above instead.
        }
        // End of copy paste from `debug_port_start` in `probe-rs/src/architecture/arm/sequences.rs`

        // This code is unique to the cc23xx_cc27xx family
        // First connect to the config AP to read the device status register
        // This will tell us the state of the boot rom and if SACI is enabled

        // Read the device status register
        let mut device_status = DeviceStatusRegister::read(interface)?;

        // AHB-AP is not accessible when in SACI mode, so exit SACI
        if !device_status.ahb_ap_available() {
            // Send the SACI command to exit SACI
            self.saci_command(interface, 0x07)?;

            // Read the device status register again to check if boot is completed
            device_status = DeviceStatusRegister::read(interface)?;

            // Check if the boot rom is waiting for a debugger to attach
            match device_status.boot_status() {
                BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE
                | BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE
                | BOOT_STATUS_APP_WAITLOOP_DBGPROBE => {
                    tracing::info!("BOOT_WAITLOOP_DBGPROBE");
                    self.boot_loop.store(true, Ordering::SeqCst);
                }
                _ => tracing::warn!("Expected device to be waiting on debugger, but it is not"),
            }
        }

        Ok(())
    }

    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.is_in_boot_loop() {
            // Step 1: Halt the CPU
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();

            let mut memory = interface.memory_interface(core_ap)?;
            memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            // Step 1.1: Wait for the CPU to halt
            dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
            while !dhcsr.s_halt() {
                dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
            }

            // Step 2: Write R3 to 0 to exit the boot loop
            cortex_m::write_core_reg(memory.deref_mut(), crate::RegisterId(3), 0x00000000)?;

            // Step 3: Clear the BOOT_LOOP flag
            self.boot_loop.store(false, Ordering::SeqCst);
        }

        // Step 4: Start the core like normal
        let mut core = interface.memory_interface(core_ap)?;
        cortex_m_core_start(&mut *core)
    }
}
