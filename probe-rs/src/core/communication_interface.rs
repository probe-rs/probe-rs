use crate::DebugProbeError;

pub trait CommunicationInterface {
    fn flush(&mut self) -> Result<(), DebugProbeError>;
}
