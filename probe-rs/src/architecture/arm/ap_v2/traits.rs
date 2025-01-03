use crate::architecture::arm::{ArmError, DapAccess, FullyQualifiedApAddress};

pub trait ApAccess {
    fn read_register<R: super::registers::Register>(
        &mut self,
        address: &FullyQualifiedApAddress,
    ) -> Result<R, ArmError>;
    fn write_register<R: super::registers::Register>(
        &mut self,
        address: &FullyQualifiedApAddress,
        reg: R,
    ) -> Result<(), ArmError>;
}

impl<T: DapAccess> ApAccess for T {
    fn read_register<R: super::registers::Register>(
        &mut self,
        address: &FullyQualifiedApAddress,
    ) -> Result<R, ArmError> {
        let raw = self.read_raw_ap_register(address, R::ADDRESS as u8)?;
        R::try_from(raw).map_err(Into::into)
    }

    fn write_register<R: super::registers::Register>(
        &mut self,
        address: &FullyQualifiedApAddress,
        reg: R,
    ) -> Result<(), ArmError> {
        self.write_raw_ap_register(address, R::ADDRESS as u8, reg.into())
    }
}
