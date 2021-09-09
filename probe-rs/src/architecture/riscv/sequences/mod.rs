use super::communication_interface::RiscvCommunicationInterface;
use std::sync::Arc;

pub mod esp32c3;

pub trait RiscvDebugSequence: Send + Sync {
    fn on_connect(&self, _interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        Ok(())
    }
}

pub struct DefaultRiscvSequence(pub(crate) ());

impl DefaultRiscvSequence {
    pub fn new() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self(()))
    }
}

impl RiscvDebugSequence for DefaultRiscvSequence {}
