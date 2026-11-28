//! Mock memory interface and DAP for core tests.

use std::collections::VecDeque;

use crate::{
    MemoryInterface,
    architecture::arm::{
        ArmError, DapAccess, FullyQualifiedApAddress,
        communication_interface::SwdSequence,
        dp::{DpAddress, DpRegisterAddress},
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
    error: Option<ArmError>,
}

/// A memory interface that expects a list of 32-bit reads and writes, in order.
///
/// The mock panics on an unexpected operation, and on drop if an expected operation did not occur.
pub struct MockProbe {
    base_address: u64,
    expected_ops: Vec<ExpectedMemoryOp>,
    /// Every call of `execute_operations`: each operation's address, and the value if it
    /// was a word write.
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
            error: None,
        });
    }

    pub fn expected_write(&mut self, addr: u64, value: u32) {
        self.expected_ops.push(ExpectedMemoryOp {
            read: false,
            address: addr,
            value,
            error: None,
        });
    }

    /// Expect a failed read or write, which may have reached the target.
    pub fn expected_error(&mut self, address: u64, read: bool, error: ArmError) {
        self.expected_ops.push(ExpectedMemoryOp {
            read,
            address,
            value: 0,
            error: Some(error),
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

        if let Some(error) = expected_op.error {
            return Err(error);
        }
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

        if let Some(error) = expected_op.error {
            return Err(error);
        }
        assert_eq!(
            expected_op.value, data[0],
            "Write value mismatch Expected {:#X} Actual: {:#X}",
            expected_op.value, data[0]
        );

        Ok(())
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
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
                        _ => None,
                    };
                    (operation.address, value)
                })
                .collect(),
        );
        for operation in operations.iter_mut() {
            let address = operation.address;
            match &mut operation.operation {
                OperationKind::Read(data) => self.read(address, data)?,
                OperationKind::Read8(data) => self.read_8(address, data)?,
                OperationKind::Read16(data) => self.read_16(address, data)?,
                OperationKind::Read32(data) => self.read_32(address, data)?,
                OperationKind::Read64(data) => self.read_64(address, data)?,
                OperationKind::Write(data) => self.write(address, data)?,
                OperationKind::Write8(data) => self.write_8(address, data)?,
                OperationKind::Write16(data) => self.write_16(address, data)?,
                OperationKind::Write32(data) => self.write_32(address, data)?,
                OperationKind::Write64(data) => self.write_64(address, data)?,
                OperationKind::WriteWord8(data) => self.write_word_8(address, *data)?,
                OperationKind::WriteWord16(data) => self.write_word_16(address, *data)?,
                OperationKind::WriteWord32(data) => self.write_word_32(address, *data)?,
                OperationKind::WriteWord64(data) => self.write_word_64(address, *data)?,
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

/// A DAP that records every batch of raw AP accesses and answers reads from a script.
pub struct RecordingDap {
    pub batches: Vec<Vec<(u64, Option<u32>)>>,
    reads: VecDeque<u32>,
    /// The batch, counted from zero, that fails with a probe error.
    pub failing_batch: Option<usize>,
}

impl RecordingDap {
    /// Creates a DAP that answers reads with `reads`, in order.
    pub fn new(reads: &[u32]) -> Self {
        Self {
            batches: Vec::new(),
            reads: reads.iter().copied().collect(),
            failing_batch: None,
        }
    }
}

impl DapAccess for RecordingDap {
    fn read_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        unimplemented!()
    }

    fn write_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _address: DpRegisterAddress,
        _value: u32,
    ) -> Result<(), ArmError> {
        unimplemented!()
    }

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        let mut value = [0];
        self.access_raw_ap_registers(ap, &[(address, None)], &mut value)?;
        Ok(value[0])
    }

    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        self.access_raw_ap_registers(ap, &[(address, Some(value))], &mut [])
    }

    fn access_raw_ap_registers(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        accesses: &[(u64, Option<u32>)],
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let index = self.batches.len();
        self.batches.push(accesses.to_vec());
        if self.failing_batch == Some(index) {
            return Err(ArmError::Probe(DebugProbeError::Other(
                "the probe went away".to_string(),
            )));
        }
        let reads = accesses.iter().filter(|(_, value)| value.is_none()).count();
        for value in &mut values[..reads] {
            *value = self
                .reads
                .pop_front()
                .expect("a scripted value for every read");
        }
        Ok(())
    }
}
