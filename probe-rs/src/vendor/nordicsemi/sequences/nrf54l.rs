//! Sequences for the nRF54L family of devices.
use std::{sync::Arc, time::Instant};

use crate::{
    architecture::arm::{
        ap::memory_ap::registers::CSW, sequences::ArmDebugSequence, ArmError,
        FullyQualifiedApAddress, Register,
    },
    session::MissingPermissions,
};

const RESET: u8 = 0;
const ERASEALL: u8 = 0x04;
const ERASEALLSTATUS: u8 = 0x08;

/// The sequence handle for the nRF5340.

#[derive(Debug)]
pub struct Nrf54L(());

impl Nrf54L {
    /// Create a new sequence handle for the nRF54L chips.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for Nrf54L {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn crate::architecture::arm::ArmProbeInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Read CSW register to see if the device is unlocked
        let csw: CSW = interface
            .read_raw_ap_register(default_ap, CSW::ADDRESS)?
            .try_into()?;

        if csw.DeviceEn {
            tracing::debug!("Core is already unlocked");
            return Ok(());
        }

        tracing::info!("Core is locked. Erase procedure will be started to unlock it.");
        permissions
            .erase_all()
            .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

        let ctrl_ap = FullyQualifiedApAddress::v1_with_dp(default_ap.dp(), 2);

        interface.write_raw_ap_register(&ctrl_ap, ERASEALL, 1)?;

        let start = Instant::now();

        let erase_all_status = loop {
            let erase_all_status = interface.read_raw_ap_register(&ctrl_ap, ERASEALLSTATUS)?;

            if erase_all_status != 2 {
                break erase_all_status;
            }

            std::thread::sleep(std::time::Duration::from_millis(1));

            if start.elapsed().as_secs() > 5 {
                return Err(ArmError::Timeout);
            }
        };

        tracing::debug!("Erase all finished with status {erase_all_status}");

        match erase_all_status {
            // Ready to reset
            1 => {
                // Trigger soft reset
                interface.write_raw_ap_register(&ctrl_ap, RESET, 2)?;

                // Release reset
                interface.write_raw_ap_register(&ctrl_ap, RESET, 0)?;
            }
            // Error
            3 => {
                return Err(ArmError::Other("Erase all failed".to_string()));
            }
            status => {
                return Err(ArmError::Other(format!(
                    "Erase all failed with unexpected status codee {}",
                    status
                )));
            }
        }

        Ok(())
    }
}
