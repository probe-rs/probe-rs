//! A probe lister for tests that wait for a busy probe.

use probe_rs::integration::{FakeProbe, FakeProbeFactory, ProbeLister};
use probe_rs::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError,
    list::ProbeListItem,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A probe that another process holds. `open` fails until `free_after`
/// attempts have been made, and while it is held the probe is missing from
/// the probe list if `listed_while_busy` is false.
pub struct BusyLister {
    pub info: DebugProbeInfo,
    probe: Mutex<Option<FakeProbe>>,
    attempts: AtomicUsize,
    free_after: usize,
    listed_while_busy: bool,
    faulty: bool,
    panicking: bool,
}

impl BusyLister {
    /// A lister that gives `probe` to the first open after `free_after` failed attempts.
    pub fn new(free_after: usize, probe: FakeProbe) -> Self {
        Self {
            info: DebugProbeInfo::new(
                "Mock probe",
                0x12,
                0x23,
                Some("busy_serial".to_owned()),
                &FakeProbeFactory,
                None,
                false,
            ),
            probe: Mutex::new(Some(probe)),
            attempts: AtomicUsize::new(0),
            free_after,
            listed_while_busy: true,
            faulty: false,
            panicking: false,
        }
    }

    /// The probe drops out of the probe list while another process holds it.
    pub fn hidden_while_busy(mut self) -> Self {
        self.listed_while_busy = false;
        self
    }

    /// The probe answers the open, then faults.
    pub fn faulty(mut self) -> Self {
        self.faulty = true;
        self
    }

    /// The probe driver panics instead of reporting the failed open.
    pub fn panicking(mut self) -> Self {
        self.panicking = true;
        self
    }

    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn busy(&self) -> bool {
        self.attempts() < self.free_after
    }
}

impl std::fmt::Debug for BusyLister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BusyLister")
    }
}

impl ProbeLister for BusyLister {
    fn open(&self, _selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.panicking {
            panic!("the probe driver gave up");
        }
        if self.faulty {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::Usb(std::io::Error::other(
                    "hardware fault or protocol violation",
                )),
            ));
        }
        if attempt < self.free_after {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ));
        }

        #[expect(
            clippy::unwrap_used,
            reason = "Test lister: a poisoned mutex is unrecoverable"
        )]
        match self.probe.lock().unwrap().take() {
            Some(probe) => Ok(Probe::from_specific_probe(Box::new(probe))),
            None => Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            )),
        }
    }

    fn list_with_access(&self, _selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        if self.busy() && !self.listed_while_busy {
            return Vec::new();
        }
        vec![ProbeListItem::accessible(self.info.clone())]
    }
}
