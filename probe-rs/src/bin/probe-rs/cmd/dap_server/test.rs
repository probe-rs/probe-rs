use std::cell::RefCell;

use probe_rs::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, FakeProbe, Probe, ProbeLister,
};

#[derive(Debug)]
pub struct TestLister {
    pub probes: RefCell<Vec<(DebugProbeInfo, FakeProbe)>>,
}

impl TestLister {
    pub fn new() -> Self {
        Self {
            probes: RefCell::new(Vec::new()),
        }
    }
}

impl ProbeLister for TestLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let probe_index = self.probes.borrow().iter().position(|(info, _)| {
            info.product_id == selector.product_id
                && info.vendor_id == selector.vendor_id
                && info.serial_number == selector.serial_number
        });

        if let Some(index) = probe_index {
            let (_info, probe) = self.probes.borrow_mut().swap_remove(index);

            Ok(Probe::from_specific_probe(Box::new(probe)))
        } else {
            Err(DebugProbeError::ProbeCouldNotBeCreated(
                probe_rs::ProbeCreationError::CouldNotOpen,
            ))
        }
    }

    fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.probes
            .borrow()
            .iter()
            .map(|(info, _)| info.clone())
            .collect()
    }
}
