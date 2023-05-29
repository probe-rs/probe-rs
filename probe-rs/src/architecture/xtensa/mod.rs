//! All the interface bits for Xtensa.

use self::communication_interface::XtensaCommunicationInterface;

mod xdm;

pub mod communication_interface;


/// A interface to operate Xtensa cores.
pub struct Xtensa<'probe> {
    interface: &'probe mut XtensaCommunicationInterface,
}

impl<'probe> Xtensa<'probe> {
    /// Create a new Xtensa interface.
    pub fn new(interface: &'probe mut XtensaCommunicationInterface) -> Self {
        Self { interface }
    }
}