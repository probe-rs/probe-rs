//! Sequences for the nRF54L family of devices.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ArmError, FullyQualifiedApAddress,
        ap::{ApRegister, CSW},
        sequences::ArmDebugSequence,
    },
    session::MissingPermissions,
};

/// CTRL-AP register offsets.
const RESET: u64 = 0x00;
const ERASEALL: u64 = 0x04;
const ERASEALLSTATUS: u64 = 0x08;

/// `CTRLAP.RESET` register values.
const RESET_NONE: u32 = 0; // NoReset
const RESET_HARD: u32 = 2; // HardReset
const RESET_PIN: u32 = 4; // PinReset

/// `CTRLAP.ERASEALLSTATUS` register values.
const ERASEALLSTATUS_READY_TO_RESET: u32 = 1;
const ERASEALLSTATUS_BUSY: u32 = 2;

/// How many times to retry the erase-all (each retry is preceded by a pin reset) before giving up.
const MAX_ERASE_ALL_ATTEMPTS: usize = 3;

/// The sequence handle for the nRF54L family.
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
        interface: &mut dyn crate::architecture::arm::ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Read CSW register to see if the device is unlocked
        let csw: CSW = interface
            .read_raw_ap_register(default_ap, CSW::ADDRESS)?
            .try_into()?;

        if csw.DeviceEn() {
            tracing::debug!("Core is already unlocked");
            return Ok(());
        }

        tracing::info!("Core is locked. Erase procedure will be started to unlock it.");
        permissions
            .erase_all()
            .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

        let ctrl_ap = FullyQualifiedApAddress::v1_with_dp(default_ap.dp(), 2);

        // Disable access port protection via a CTRL-AP ERASEALL, following the "Disabling
        // APPROTECT" procedure from the nRF54L documentation.
        for attempt in 1..=MAX_ERASE_ALL_ATTEMPTS {
            // Start the erase.
            interface.write_raw_ap_register(&ctrl_ap, ERASEALL, 1)?;

            // Wait for ERASEALLSTATUS to change from Busy.
            let start = Instant::now();
            let erase_all_status = loop {
                let erase_all_status = interface.read_raw_ap_register(&ctrl_ap, ERASEALLSTATUS)?;

                if erase_all_status != ERASEALLSTATUS_BUSY {
                    break erase_all_status;
                }

                std::thread::sleep(Duration::from_millis(1));

                if start.elapsed().as_secs() > 5 {
                    return Err(ArmError::Timeout);
                }
            };

            tracing::debug!("Erase all finished with status {erase_all_status}");

            if erase_all_status == ERASEALLSTATUS_READY_TO_RESET {
                // The erase succeeded. Apply it with a hard reset to complete unlocking.
                interface.write_raw_ap_register(&ctrl_ap, RESET, RESET_HARD)?;
                interface.write_raw_ap_register(&ctrl_ap, RESET, RESET_NONE)?;

                return Ok(());
            }

            // Any other status means the erase failed. The documented recovery is to do a pin
            // reset and retry the whole procedure.
            // NOTE: Nordic's "nRF54L Series Production Programming" docs say any *non-zero* status
            // is an error and should be recovered by pin reset, but doesn't say what to do if status is zero.
            // We treat zero as an error here.
            tracing::warn!(
                "Erase all failed with status {erase_all_status}, \
                 doing a pin reset and retrying (attempt {attempt}/{MAX_ERASE_ALL_ATTEMPTS})"
            );
            interface.write_raw_ap_register(&ctrl_ap, RESET, RESET_PIN)?;
            interface.write_raw_ap_register(&ctrl_ap, RESET, RESET_NONE)?;
        }

        Err(ArmError::Other(
            "Erase all failed: could not unlock the device".to_string(),
        ))
    }
}
