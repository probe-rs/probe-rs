//! Xtensa Debug Module Communication

// TODO: remove
#![allow(missing_docs)]

use crate::{
    architecture::xtensa::arch::instruction, probe::JTAGAccess, DebugProbeError, MemoryInterface,
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error: {0}")]
    XdmError(XdmError),
}

/// A interface that implements controls for Xtensa cores.
#[derive(Debug)]
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    xdm: Xdm,
    // state: XtensaCommunicationInterfaceState,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| match e {
            XtensaError::DebugProbe(err) => (probe, err),
            other_error => (probe, DebugProbeError::Other(other_error.into())),
        })?;

        let s = Self { xdm };

        Ok(s)
    }

    pub fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.enter_ocd_mode()?;
        tracing::info!("Entered OCD mode");
        Ok(())
    }

    pub fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_in_ocd_mode()
    }

    pub fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.leave_ocd_mode()?;
        tracing::info!("Left OCD mode");
        Ok(())
    }

    pub fn halt(&mut self) -> Result<(), XtensaError> {
        self.xdm.halt()?;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), XtensaError> {
        self.xdm.resume()?;
        Ok(())
    }
}

impl MemoryInterface for XtensaCommunicationInterface {
    fn read(&mut self, address: u64, dst: &mut [u8]) -> Result<(), crate::Error> {
        if dst.is_empty() {
            return Ok(());
        }

        let read_words = if dst.len() % 4 == 0 {
            dst.len() / 4
        } else {
            dst.len() / 4 + 1
        };

        const SR_DDR: u8 = 104;
        const A3: u8 = 3;

        const MOVE_A3_TO_DDR: u32 = instruction::rsr(SR_DDR, A3);
        const MOVE_DDR_TO_A3: u32 = instruction::wsr(A3, SR_DDR);
        const READ_DATA_FROM: u32 = instruction::lddr32_p(A3);

        // Save A3
        // TODO: mark in restore context
        self.xdm.execute_instruction(MOVE_A3_TO_DDR)?;
        let old_a3 = self.xdm.read_ddr()?;

        // Write address to A3
        self.xdm.write_ddr(address as u32)?;
        self.xdm.execute_instruction(MOVE_DDR_TO_A3)?;

        // Read from address in A3
        self.xdm.execute_instruction(READ_DATA_FROM)?;

        for i in 0..read_words - 1 {
            let word = self.xdm.read_ddr_and_execute()?.to_le_bytes();
            dst[4 * i..][..4].copy_from_slice(&word);
        }

        let remaining_bytes = if dst.len() % 4 == 0 { 4 } else { dst.len() % 4 };

        let word = self.xdm.read_ddr()?;
        dst[4 * (read_words - 1)..][..remaining_bytes]
            .copy_from_slice(&word.to_le_bytes()[..remaining_bytes]);

        // Restore A3
        // TODO: only do this when restoring program context
        self.xdm.write_ddr(old_a3)?;
        self.xdm.execute_instruction(MOVE_DDR_TO_A3)?;

        Ok(())
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut out = [0; 4];
        self.read(address, &mut out)?;

        Ok(u32::from_le_bytes(out))
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> anyhow::Result<u64, crate::Error> {
        todo!()
    }

    fn read_word_8(&mut self, address: u64) -> anyhow::Result<u8, crate::Error> {
        todo!()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
        todo!()
    }

    fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
        todo!()
    }
}
