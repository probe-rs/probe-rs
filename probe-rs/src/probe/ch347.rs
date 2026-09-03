//! CH347: a USB bridge with UART, I2C, SPI, GPIO and a JTAG/SWD engine.
mod capabilities;
mod device;
mod jtag;
mod swd;
mod transport;

use std::sync::Arc;

use bitvec::vec::BitVec;
use device::Ch347Device;

use crate::{
    architecture::{
        arm::{
            ArmCommunicationInterface, ArmDebugInterface, ArmError, sequences::ArmDebugSequence,
        },
        riscv::{
            communication_interface::{RiscvError, RiscvInterfaceBuilder},
            dtm::jtag_dtm::JtagDtmBuilder,
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState, XtensaError,
        },
    },
    probe::{
        BatchExecutionError, BitbangJtag, DebugProbe, DebugProbeError, DebugProbeSelector,
        JtagChain, JtagChainAccess, JtagChainState, ProbeFactory, Results, SwdBatch, SwdProbe,
        TapState, WireProtocol, list::ProbeListItem,
    },
};

/// A factory for creating [`Ch347`] instances.
#[derive(Debug)]
pub struct Ch347Factory;

impl std::fmt::Display for Ch347Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CH347")
    }
}

/// A CH347-based debug probe.
///
/// JTAG is bit-banged; SWD runs on the chip's transaction engine.
#[derive(Debug)]
pub struct Ch347 {
    device: Ch347Device,
    jtag_state: JtagChainState,
}

impl ProbeFactory for Ch347Factory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let device = Ch347Device::new_from_selector(selector)?;

        tracing::info!("Found ch347 device");
        Ok(Box::new(Ch347 {
            device,
            jtag_state: JtagChainState::default(),
        }))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        device::list_ch347_devices()
    }
}

impl Ch347 {
    fn jtag(&self) -> bool {
        self.device.protocol() == WireProtocol::Jtag
    }

    fn no_jtag() -> DebugProbeError {
        DebugProbeError::InterfaceNotAvailable {
            interface_name: "JTAG",
        }
    }
}

impl BitbangJtag for Ch347 {
    fn tap_state(&mut self) -> &mut TapState {
        &mut self.jtag_state.tap_state
    }

    fn shift(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.device.shift_bit(tms, tdi, capture)
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.device.flush_jtag()
    }

    fn captured(&mut self) -> Result<BitVec, DebugProbeError> {
        self.device.read_captured_bits()
    }
}

impl JtagChainAccess for Ch347 {
    fn chain_state(&mut self) -> &mut JtagChainState {
        &mut self.jtag_state
    }

    fn chain_state_ref(&self) -> &JtagChainState {
        &self.jtag_state
    }
}

impl SwdProbe for Ch347 {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        self.device.run_swd_batch(batch)
    }

    /// The firmware runs a whole batch regardless of the ACKs, so a replay from the WAITed
    /// access would repeat the accesses after it; the engine retries on its own terms.
    fn handles_wait(&self) -> bool {
        true
    }

    fn handles_ap_pipeline(&self) -> bool {
        true
    }
}

impl DebugProbe for Ch347 {
    fn get_name(&self) -> &str {
        "CH347"
    }

    fn speed_khz(&self) -> u32 {
        self.device.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.device.set_speed(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.device.attach()
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(self.device.detach()?)
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset_deassert",
        })
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        self.device.select_protocol(protocol)
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(self.device.protocol())
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        self.jtag().then(|| JtagChain::new(self))
    }

    fn try_as_jtag_chain_access_mut(&mut self) -> Option<&mut dyn JtagChainAccess> {
        self.jtag().then_some(self)
    }

    fn try_as_swd_probe(self: Box<Self>) -> Result<Box<dyn SwdProbe>, Box<dyn DebugProbe>> {
        if self.jtag() { Err(self) } else { Ok(self) }
    }

    fn try_as_swd_probe_mut(&mut self) -> Option<&mut dyn SwdProbe> {
        (!self.jtag()).then_some(self)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        let settings = SwdProbe::swd_settings(self.as_ref());
        Ok(match self.device.protocol() {
            WireProtocol::Jtag => {
                ArmCommunicationInterface::create_jtag(self, settings, sequence, true)
            }
            // This firmware mishandles the data phase that overrun detection adds to a WAIT.
            WireProtocol::Swd => {
                ArmCommunicationInterface::create_swd(self, settings, sequence, false)
            }
        })
    }

    fn has_riscv_interface(&self) -> bool {
        self.jtag()
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, RiscvError> {
        if !self.jtag() {
            return Err(Self::no_jtag().into());
        }
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_xtensa_interface(&self) -> bool {
        self.jtag()
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, XtensaError> {
        if !self.jtag() {
            return Err(Self::no_jtag().into());
        }
        Ok(XtensaCommunicationInterface::new(self, state))
    }
}
