//! Generic access port
use crate::architecture::arm::{
    ap::{AccessPortType, ApRegAccess, IDR},
    FullyQualifiedApAddress,
};

/// A generic access port which implements just the register every access port has to implement
/// to be compliant with the ADI 5.2 specification.
#[derive(Clone, Debug)]
pub struct GenericAp {
    address: FullyQualifiedApAddress,
}

impl GenericAp {
    /// Creates a new GenericAp #[doc = concat!("Creates a new ", stringify!($name), " with `address` as base address.")]
    pub const fn new(address: FullyQualifiedApAddress) -> Self {
        Self { address }
    }
}

impl AccessPortType for GenericAp {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}
impl ApRegAccess<IDR> for GenericAp {}
