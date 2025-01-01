use parking_lot::Mutex;
use probe_rs::{
    integration::{FakeProbe, ProbeLister},
    probe::{DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError},
};

#[derive(Debug)]
pub struct TestLister {
    pub probes: Mutex<Vec<(DebugProbeInfo, FakeProbe)>>,
}

impl TestLister {
    pub fn new() -> Self {
        Self {
            probes: Mutex::new(Vec::new()),
        }
    }
}

impl ProbeLister for TestLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let probe_index = self.probes.lock().iter().position(|(info, _)| {
            info.product_id == selector.product_id
                && info.vendor_id == selector.vendor_id
                && info.serial_number == selector.serial_number
        });

        if let Some(index) = probe_index {
            let (_info, probe) = self.probes.lock().swap_remove(index);

            Ok(Probe::from_specific_probe(Box::new(probe)))
        } else {
            Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ))
        }
    }

    fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.probes
            .lock()
            .iter()
            .map(|(info, _)| info.clone())
            .collect()
    }
}
