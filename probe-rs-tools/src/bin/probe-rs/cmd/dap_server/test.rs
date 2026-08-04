use std::sync::Mutex;

use probe_rs::{
    integration::{FakeProbe, ProbeLister},
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError,
        ProbeFactory, list::ProbeListItem,
    },
};
use std::fmt::Display;

#[derive(Debug)]
pub struct MockProbeFactory;

impl Display for MockProbeFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mocked Probe")
    }
}

impl ProbeFactory for MockProbeFactory {
    fn open(&self, _selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        unreachable!("TestLister opens FakeProbe directly")
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        Vec::new()
    }
}

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
        #[expect(
            clippy::unwrap_used,
            reason = "Test lister: a poisoned mutex is unrecoverable"
        )]
        let mut probes = self.probes.lock().unwrap();
        let probe_index = probes.iter().position(|(info, _)| {
            info.product_id == selector.product_id
                && info.vendor_id == selector.vendor_id
                && info.serial_number == selector.serial_number
        });

        if let Some(index) = probe_index {
            let (_info, probe) = probes.swap_remove(index);

            Ok(Probe::from_specific_probe(Box::new(probe)))
        } else {
            Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ))
        }
    }

    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        #[expect(
            clippy::unwrap_used,
            reason = "Test lister: a poisoned mutex is unrecoverable"
        )]
        let probes = self.probes.lock().unwrap();
        probes
            .iter()
            .filter_map(|(info, _)| {
                if selector
                    .as_ref()
                    .is_none_or(|selector| selector.matches_probe(info))
                {
                    Some(ProbeListItem::accessible(info.clone()))
                } else {
                    None
                }
            })
            .collect()
    }
}
