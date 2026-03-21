//! Debug sequences for PSOC Edge devices.
use std::sync::Arc;

use bitfield::bitfield;
use probe_rs_target::{Chip, CoreType};

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        sequences::{ArmDebugSequence, DefaultArmSequence},
    },
    config::CoreExt,
};

/// PSOC Edge debug sequences.
#[derive(Debug)]
pub struct PsocEdge {
    /// The cores present on the device.
    cores: Vec<probe_rs_target::Core>,
}

impl PsocEdge {
    /// Tries to create debug sequences for a PSOC Edge chip.
    pub fn create(chip: &Chip) -> Arc<Self> {
        Arc::new(PsocEdge {
            cores: chip.cores.clone(),
        })
    }
}

impl ArmDebugSequence for PsocEdge {
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        bitfield! {
            /// CM55 control register.
            #[derive(Clone, Copy)]
            struct MxCm55Ctl(u32);
            impl Debug;

            pub cm55_wait, set_cm55_wait: 4;
        }
        impl MxCm55Ctl {
            const ADDRESS: u64 = 0x44160000;
        }

        #[derive(Debug, Clone, Copy)]
        // Power Domain Dependency Sense register for power domain 6 (APPCPU).
        struct AppCpuPdSense;
        impl AppCpuPdSense {
            const ADDRESS: u64 = 0x42410060;
        }

        // Power domain ID for the CM33.
        const SYSCPU_PD: u32 = 4;
        bitfield! {
            /// CM55 access port control register.
            #[derive(Clone, Copy)]
            struct AppCpussApCtl(u32);
            impl Debug;

            pub cm55_enable, set_cm55_enable: 0;
            pub cm55_dbg_enable, set_cm55_dbg_enable: 4;
            pub cm55_nid_enable, set_cm55_nid_enable: 5;
        }
        impl AppCpussApCtl {
            const ADDRESS: u64 = 0x441C1000;
        }

        let [cm33, cm55] = &*self.cores else {
            unreachable!("PSOC Edge E84 devices have two cores");
        };
        let [cm33_ap, cm55_ap] = [cm33, cm55].map(|core| {
            core.memory_ap()
                .expect("PSOC Edge core must have a memory AP")
        });

        if core_ap == &cm55_ap {
            // In order to debug the CM55, we may need to first power it up using the CM33..
            let mut cm33_ap = interface.memory_interface(&cm33_ap)?;

            // Check if the CM55 is enabled.
            let ctl = MxCm55Ctl(cm33_ap.read_word_32(MxCm55Ctl::ADDRESS)?);
            if ctl.cm55_wait() {
                return Err(ArmError::CoreDisabled);
            }

            // Ensure the CM55 is powered up while the CM33 is powered (by the debug port)
            let pd_sense: u32 = cm33_ap.read_word_32(AppCpuPdSense::ADDRESS)?;
            if (pd_sense & (1 << SYSCPU_PD)) == 0 {
                cm33_ap.write_word_32(AppCpuPdSense::ADDRESS, pd_sense | (1 << SYSCPU_PD))?;
            }

            // Enable the CM55 AP.
            let mut ap_ctl = AppCpussApCtl(cm33_ap.read_word_32(AppCpussApCtl::ADDRESS)?);
            ap_ctl.set_cm55_enable(true);
            ap_ctl.set_cm55_dbg_enable(true);
            ap_ctl.set_cm55_nid_enable(true);
            cm33_ap.write_word_32(AppCpussApCtl::ADDRESS, ap_ctl.0)?;
        }

        DefaultArmSequence(()).on_attach(interface, core_ap, core_type)
    }
}
