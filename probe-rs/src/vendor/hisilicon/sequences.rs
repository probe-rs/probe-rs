use std::{sync::Arc, thread, time::Duration};

use crate::{
    Permissions,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress,
        dp::{DpAddress, DpRegisterAddress},
        sequences::ArmDebugSequence,
    },
};

const DP_ABORT: DpRegisterAddress = DpRegisterAddress {
    address: 0,
    bank: None,
};
const VENDOR_UNGATE: DpRegisterAddress = DpRegisterAddress {
    address: 0x4,
    bank: Some(0xe),
};

fn ungate_debug_module(interface: &mut dyn DapAccess, dp: DpAddress) -> Result<u32, ArmError> {
    interface.write_raw_dp_register(dp, DP_ABORT, u32::MAX)?;
    interface.write_raw_dp_register(dp, VENDOR_UNGATE, 3)?;
    let value = interface.read_raw_dp_register(dp, VENDOR_UNGATE)?;
    interface.flush()?;
    Ok(value)
}

#[derive(Debug)]
pub struct HiSilicon3322Sequence;

impl HiSilicon3322Sequence {
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self)
    }
}

impl ArmDebugSequence for HiSilicon3322Sequence {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &Permissions,
    ) -> Result<(), ArmError> {
        let value = ungate_debug_module(interface, default_ap.dp())?;
        tracing::debug!(value, "HiSilicon 3322 debug module ungated");
        thread::sleep(Duration::from_millis(50));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Operation {
        Write(DpRegisterAddress, u32),
        Read(DpRegisterAddress),
        Flush,
    }

    #[derive(Default)]
    struct MockDap {
        operations: Vec<Operation>,
    }

    impl DapAccess for MockDap {
        fn read_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            addr: DpRegisterAddress,
        ) -> Result<u32, ArmError> {
            self.operations.push(Operation::Read(addr));
            Ok(3)
        }

        fn write_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            addr: DpRegisterAddress,
            value: u32,
        ) -> Result<(), ArmError> {
            self.operations.push(Operation::Write(addr, value));
            Ok(())
        }

        fn read_raw_ap_register(
            &mut self,
            _ap: &FullyQualifiedApAddress,
            _addr: u64,
        ) -> Result<u32, ArmError> {
            unreachable!()
        }

        fn write_raw_ap_register(
            &mut self,
            _ap: &FullyQualifiedApAddress,
            _addr: u64,
            _value: u32,
        ) -> Result<(), ArmError> {
            unreachable!()
        }

        fn flush(&mut self) -> Result<(), ArmError> {
            self.operations.push(Operation::Flush);
            Ok(())
        }
    }

    #[test]
    fn ungates_debug_module_in_vendor_order() {
        let mut dap = MockDap::default();

        assert_eq!(
            ungate_debug_module(&mut dap, DpAddress::Default).unwrap(),
            3
        );
        assert_eq!(
            dap.operations,
            [
                Operation::Write(DP_ABORT, u32::MAX),
                Operation::Write(VENDOR_UNGATE, 3),
                Operation::Read(VENDOR_UNGATE),
                Operation::Flush,
            ]
        );
    }
}
