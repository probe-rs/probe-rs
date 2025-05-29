use probe_rs::{
    integration::{FakeProbe, ProbeLister},
    probe::{DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError},
};
use tokio::sync::Mutex;

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

#[async_trait::async_trait]
impl ProbeLister for TestLister {
    async fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let probe_index = self
            .probes
            .lock()
            .await
            .iter()
            .position(|(info, _)| selector.matches_probe(info));

        if let Some(index) = probe_index {
            let (_info, probe) = self.probes.lock().await.swap_remove(index);

            Ok(Probe::from_specific_probe(Box::new(probe)))
        } else {
            Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ))
        }
    }

    async fn list(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        self.probes
            .lock()
            .await
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
