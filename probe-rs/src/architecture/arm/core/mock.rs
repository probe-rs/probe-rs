//! Mock memory interface for core tests.

use crate::{
    MemoryInterface,
    architecture::arm::{
        ArmError, FullyQualifiedApAddress, communication_interface::SwdSequence,
        memory::ArmMemoryInterface,
    },
    memory::{Operation, OperationKind},
    probe::{BitSequence, DebugProbeError},
};

#[derive(Debug)]
struct ExpectedMemoryOp {
    read: bool,
    address: u64,
    value: u32,
}

/// A memory interface that expects a list of 32-bit reads and writes, in order.
///
/// The mock panics on an unexpected operation, and on drop if an expected operation did not occur.
pub struct MockProbe {
    base_address: u64,
    expected_ops: Vec<ExpectedMemoryOp>,
    /// Every call of `execute_operations`: each operation's address, and the value if it
    /// was a write.
    pub batches: Vec<Vec<(u64, Option<u32>)>>,
}

impl MockProbe {
    /// Creates a mock. Error messages show addresses as register numbers from `base_address`.
    pub fn new(base_address: u64) -> Self {
        MockProbe {
            base_address,
            expected_ops: vec![],
            batches: vec![],
        }
    }

    pub fn expected_read(&mut self, addr: u64, value: u32) {
        self.expected_ops.push(ExpectedMemoryOp {
            read: true,
            address: addr,
            value,
        });
    }

    pub fn expected_write(&mut self, addr: u64, value: u32) {
        self.expected_ops.push(ExpectedMemoryOp {
            read: false,
            address: addr,
            value,
        });
    }

    fn address_to_reg_num(&self, address: u64) -> u32 {
        ((address - self.base_address) / 4) as u32
    }
}

impl MemoryInterface<ArmError> for MockProbe {
    fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        if self.expected_ops.is_empty() {
            panic!(
                "Received unexpected read_32 op: register {:#}",
                self.address_to_reg_num(address)
            );
        }

        assert_eq!(data.len(), 1);

        let expected_op = self.expected_ops.remove(0);

        assert!(
            expected_op.read,
            "R/W mismatch for register: Expected {:#} Actual: {:#}",
            self.address_to_reg_num(expected_op.address),
            self.address_to_reg_num(address)
        );
        assert_eq!(
            expected_op.address,
            address,
            "Read from unexpected register: Expected {:#} Actual: {:#}",
            self.address_to_reg_num(expected_op.address),
            self.address_to_reg_num(address)
        );

        data[0] = expected_op.value;

        Ok(())
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        if self.expected_ops.is_empty() {
            panic!(
                "Received unexpected write_32 op: register {:#}",
                self.address_to_reg_num(address)
            );
        }

        assert_eq!(data.len(), 1);

        let expected_op = self.expected_ops.remove(0);

        assert!(
            !expected_op.read,
            "Read/write mismatch on register: {:#}",
            self.address_to_reg_num(address)
        );
        assert_eq!(
            expected_op.address,
            address,
            "Write to unexpected register: Expected {:#} Actual: {:#}",
            self.address_to_reg_num(expected_op.address),
            self.address_to_reg_num(address)
        );

        assert_eq!(
            expected_op.value, data[0],
            "Write value mismatch Expected {:#X} Actual: {:#X}",
            expected_op.value, data[0]
        );

        Ok(())
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        todo!()
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(false)
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }
}

impl ArmMemoryInterface for MockProbe {
    /// Records the batch, then performs it one operation at a time like the default.
    fn execute_operations(&mut self, operations: &mut [Operation<'_>]) -> Result<(), ArmError> {
        self.batches.push(
            operations
                .iter()
                .map(|operation| {
                    let value = match operation.operation {
                        OperationKind::WriteWord32(value) => Some(value),
                        OperationKind::Read32(_) => None,
                        _ => unimplemented!("only word writes and reads are mocked"),
                    };
                    (operation.address, value)
                })
                .collect(),
        );
        for operation in operations.iter_mut() {
            let address = operation.address;
            match &mut operation.operation {
                OperationKind::WriteWord32(value) => self.write_32(address, &[*value])?,
                OperationKind::Read32(data) => self.read_32(address, data)?,
                _ => unimplemented!("only word writes and reads are mocked"),
            }
            operation.result = Some(Ok(()));
        }
        Ok(())
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        todo!()
    }

    fn get_arm_debug_interface(
        &mut self,
    ) -> Result<&mut dyn crate::architecture::arm::ArmDebugInterface, DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "get_arm_debug_interface",
        })
    }

    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError> {
        Err(ArmError::Probe(DebugProbeError::NotImplemented {
            function_name: "generic_status",
        }))
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        todo!()
    }
}

impl SwdSequence for MockProbe {
    fn swj_sequence(&mut self, _bits: &BitSequence) -> Result<(), DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        todo!()
    }
}

impl Drop for MockProbe {
    fn drop(&mut self) {
        // A second panic during unwinding aborts the test run.
        if !self.expected_ops.is_empty() && !std::thread::panicking() {
            panic!("self.expected_ops is not empty: {:?}", self.expected_ops);
        }
    }
}
