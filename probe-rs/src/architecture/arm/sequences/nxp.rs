use std::{
    thread,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ap::ApAccess,
        ap::IDR,
        dp::{Abort, Ctrl, DpAccess, Select, DPIDR},
        DapAccess,
    },
    core::CoreRegister,
    DebugProbeError,
};

use super::ArmDebugSequence;

pub struct LPC55S69 {}

impl ArmDebugSequence for LPC55S69 {
    fn debug_port_start(&self, memory: &mut crate::Memory) -> Result<(), crate::Error> {
        let interface = memory.get_arm_interface()?;

        log::info!("debug_port_start");

        interface
            .write_dp_register(Select(0))
            .map_err(DebugProbeError::from)?;

        //let powered_down = interface.read_dp_register::<Select>::()

        let ctrl = interface
            .read_dp_register::<Ctrl>()
            .map_err(DebugProbeError::from)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);

            interface
                .write_dp_register(ctrl)
                .map_err(DebugProbeError::from)?;

            let start = Instant::now();

            let mut timeout = true;

            while start.elapsed() < Duration::from_micros(100_0000) {
                let ctrl = interface
                    .read_dp_register::<Ctrl>()
                    .map_err(DebugProbeError::from)?;

                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    timeout = false;
                    break;
                }
            }

            if timeout {
                return Err(DebugProbeError::Timeout.into());
            }

            // TODO: Handle JTAG Specific part

            // TODO: Only run the following code when the SWD protocol is used

            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            let mut ctrl = Ctrl(0);

            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);

            ctrl.set_mask_lane(0b1111);

            interface
                .write_dp_register(ctrl)
                .map_err(DebugProbeError::from)?;

            let mut abort = Abort(0);

            abort.set_orunerrclr(true);
            abort.set_wderrclr(true);
            abort.set_stkerrclr(true);
            abort.set_stkcmpclr(true);

            interface
                .write_dp_register(abort)
                .map_err(DebugProbeError::from)?;

            enable_debug_mailbox(memory)?;
        }

        Ok(())
    }

    fn reset_catch_set(&self, interface: &mut crate::Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::{Demcr, Dhcsr};

        let mut reset_vector = 0xffff_ffff;
        let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::ADDRESS, demcr.into())?;

        // Write some stuff
        interface.write_word_32(0x40034010, 0x00000000)?; // Program Flash Word Start Address to 0x0 to read reset vector (STARTA)
        interface.write_word_32(0x40034014, 0x00000000)?; // Program Flash Word Stop Address to 0x0 to read reset vector (STOPA)
        interface.write_word_32(0x40034080, 0x00000000)?; // DATAW0: Prepare for read
        interface.write_word_32(0x40034084, 0x00000000)?; // DATAW1: Prepare for read
        interface.write_word_32(0x40034088, 0x00000000)?; // DATAW2: Prepare for read
        interface.write_word_32(0x4003408C, 0x00000000)?; // DATAW3: Prepare for read
        interface.write_word_32(0x40034090, 0x00000000)?; // DATAW4: Prepare for read
        interface.write_word_32(0x40034094, 0x00000000)?; // DATAW5: Prepare for read
        interface.write_word_32(0x40034098, 0x00000000)?; // DATAW6: Prepare for read
        interface.write_word_32(0x4003409C, 0x00000000)?; // DATAW7: Prepare for read

        interface.write_word_32(0x40034FE8, 0x0000000F)?; // Clear FLASH Controller Status (INT_CLR_STATUS)
        interface.write_word_32(0x40034000, 0x00000003)?; // Read single Flash Word (CMD_READ_SINGLE_WORD)

        let start = Instant::now();

        let mut timeout = true;

        while start.elapsed() < Duration::from_micros(10_0000) {
            let value = interface.read_word_32(0x40034FE0)?;

            if (value & 0x4) == 0x4 {
                timeout = false;
                break;
            }
        }

        if timeout {
            log::warn!("Failed: Wait for flash word read to finish");
            return Err(crate::Error::Probe(DebugProbeError::Timeout));
        }

        if (interface.read_word_32(0x4003_4fe0)? & 0xB) == 0 {
            log::info!("No Error reading Flash Word with Reset Vector");

            reset_vector = interface.read_word_32(0x0000_0004)?;
        }

        if reset_vector != 0xffff_ffff {
            log::info!("Breakpoint on user application reset vector");

            interface.write_word_32(0xE000_2008, reset_vector | 1)?;
            interface.write_word_32(0xE000_2000, 3)?;
        }

        if reset_vector == 0xffff_ffff {
            log::info!("Enable reset vector catch");

            let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

            demcr.set_vc_corereset(true);

            interface.write_word_32(Demcr::ADDRESS, demcr.into())?;
        }

        let _ = interface.read_word_32(Dhcsr::ADDRESS)?;

        log::debug!("reset_catch_set -- done");

        Ok(())
    }

    fn reset_catch_clear(&self, interface: &mut crate::Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::Demcr;

        interface.write_word_32(0xE000_2008, 0x0)?;
        interface.write_word_32(0xE000_2000, 0x2)?;

        let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::ADDRESS, demcr.into())
    }

    fn reset_system(&self, interface: &mut crate::Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::m4::Aircr;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        let result = interface.write_word_32(Aircr::ADDRESS, aircr.into());

        if let Err(e) = result {
            log::debug!("Error requesting reset: {:?}", e);
        }

        log::info!("Waiting after reset");
        thread::sleep(Duration::from_millis(10));

        wait_for_stop_after_reset(interface)
    }
}

fn wait_for_stop_after_reset(memory: &mut crate::Memory) -> Result<(), crate::Error> {
    use crate::architecture::arm::core::m4::Dhcsr;
    log::info!("Wait for stop after reset");

    thread::sleep(Duration::from_millis(10));

    enable_debug_mailbox(memory)?;

    let mut timeout = true;

    let start = Instant::now();

    while start.elapsed() < Duration::from_micros(50_0000) {
        let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

        if !dhcsr.s_reset_st() {
            timeout = false;
            break;
        }
    }

    if timeout {
        return Err(crate::Error::Probe(DebugProbeError::Timeout));
    }

    let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

    if !dhcsr.s_halt() {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);

        memory.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;
    }

    Ok(())
}

pub fn enable_debug_mailbox(memory: &mut crate::Memory) -> Result<(), crate::Error> {
    let interface = memory.get_arm_interface()?;

    log::info!("LPC55xx connect srcipt start");

    let status: IDR = interface.read_ap_register(2)?;

    //let status = read_ap(interface, 2, 0xFC)?;

    log::info!("APIDR: {:?}", status);
    log::info!("APIDR: 0x{:08X}", u32::from(status));

    let status: u32 = interface
        .read_dp_register::<DPIDR>()
        .map_err(DebugProbeError::from)?
        .into();

    log::info!("DPIDR: 0x{:08X}", status);

    // Active DebugMailbox
    interface.write_raw_ap_register(2, 0x0, 0x0000_0021)?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_micros(30000));

    let _ = interface.read_raw_ap_register(2, 0)?;

    // Enter Debug session
    interface.write_raw_ap_register(2, 0x4, 0x0000_0007)?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_micros(30000));

    let _ = interface.read_raw_ap_register(2, 8)?;

    log::info!("LPC55xx connect srcipt end");
    Ok(())
}
