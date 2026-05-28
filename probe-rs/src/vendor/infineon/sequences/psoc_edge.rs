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

bitfield! {
    /// CM55 control register.
    #[derive(Clone, Copy)]
    struct MxCm55Ctl(u32);
    impl Debug;

    /// Clock-gates the CM55, preventing it from starting until the CM33
    /// has set up the system and initialized the CM55's vector table.
    pub cm55_wait, set_cm55_wait: 4;
}
impl MxCm55Ctl {
    const ADDRESS: u64 = 0x44160000;
}

bitfield! {
    /// Application CPU subsystem access port control register.
    #[derive(Clone, Copy)]
    struct AppCpussApCtl(u32);
    impl Debug;

    /// Enables the CM55 debug access port.
    pub cm55_enable, set_cm55_enable: 0;

    /// Enables invasive debug access to the CM55 (halting, stepping, breakpooints).
    pub cm55_dbg_enable, set_cm55_dbg_enable: 4;

    /// Enables non-invasive debug access to the CM55 (tracing and memory access).
    pub cm55_nid_enable, set_cm55_nid_enable: 5;
}
impl AppCpussApCtl {
    const ADDRESS: u64 = 0x441C1000;
}

/// PSOC Edge debug sequences.
#[derive(Debug)]
pub struct PsocEdge {
    // The access port for the system CPU (CM33).
    cm33_ap: FullyQualifiedApAddress,

    // The access port for the application CPU (CM55).
    cm55_ap: FullyQualifiedApAddress,
}

impl PsocEdge {
    /// Creates debug sequences for a PSOC Edge chip.
    pub fn create(chip: &Chip) -> Arc<Self> {
        let [cm33, cm55] = &*chip.cores else {
            unreachable!("PSOC Edge E84 devices have two cores");
        };
        let [cm33_ap, cm55_ap] = [cm33, cm55].map(|core| {
            core.memory_ap()
                .expect("PSOC Edge core must have a memory AP")
        });

        Arc::new(PsocEdge { cm33_ap, cm55_ap })
    }
}

impl ArmDebugSequence for PsocEdge {
    fn on_attach(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        if core_ap == &self.cm55_ap {
            // In order to debug the CM55, we may need to first power it up using the CM33.
            let mut cm33_ap = interface.memory_interface(&self.cm33_ap)?;

            // Check if the CM55 is enabled.
            let ctl = MxCm55Ctl(cm33_ap.read_word_32(MxCm55Ctl::ADDRESS)?);
            if ctl.cm55_wait() {
                return Err(ArmError::CoreDisabled);
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

#[cfg(test)]
mod tests {
    use super::PsocEdge;
    use crate::config::Registry;

    #[test]
    fn validate_psoc_edge_targets() {
        let registry = Registry::from_builtin_families();
        let family = registry
            .families()
            .iter()
            .find(|family| family.name == "psoc_e84")
            .unwrap();
        for chip in family.variants() {
            _ = PsocEdge::create(chip);
        }
    }
}
