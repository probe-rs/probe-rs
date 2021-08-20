use crate::{
    architecture::arm::{communication_interface::Initialized, ArmCommunicationInterface},
    DebugProbeError, Error,
};

pub trait CommunicationInterface {
    fn flush(&mut self) -> Result<(), DebugProbeError>;

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, Error>;
}
