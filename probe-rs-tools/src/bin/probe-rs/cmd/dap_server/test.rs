use std::cell::RefCell;

use probe_rs::{
    integration::{FakeProbe, ProbeLister},
    probe::{DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError},
};

use anyhow::anyhow;

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

#[async_trait::async_trait(?Send)]
impl ProbeLister for TestLister {
    async fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
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
                ProbeCreationError::CouldNotOpen(anyhow!("No pid:vid:serial pair matched")),
            ))
        }
    }

    async fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        self.probes
            .borrow()
            .iter()
            .filter_map(|(info, _)| {
                if selector
                    .as_ref()
                    .is_none_or(|selector| selector.matches_probe(info))
                {
                    Some(info.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}
