use crate::{
    architecture::arm::{
        communication_interface::Initialized, ArmCommunicationInterface, ArmError,
    },
    DebugProbeError,
};

/// A helper trait to get more specific interfaces.
pub trait CommunicationInterface {
    /// Flush all remaining commands if the target driver implements batching.
    fn flush(&mut self) -> Result<(), ArmError>;

    /// Tries to get the underlying [`ArmCommunicationInterface`].
    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, DebugProbeError>;
}
