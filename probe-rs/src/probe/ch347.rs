//! CH347: a USB bridge with UART, I2C, SPI, GPIO and a JTAG/SWD engine.
mod board;
mod capabilities;
mod device;
mod jtag;
mod swd;
mod transport;

use std::{str::FromStr, sync::Arc};

use device::Ch347Device;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
        BatchExecutionError, DebugProbe, DebugProbeError, DebugProbeSelector, JtagBatch, JtagChain,
        JtagChainAccess, JtagChainState, JtagProbe, ProbeFactory, Results, SwdBatch, SwdProbe,
        WireProtocol, list::ProbeListItem,
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
/// JTAG runs on the chip's byte shifts and bit ops, SWD on its transaction engine. Target
/// reset uses the GPIO wiring of a board the driver knows, recognised by its USB identity, or
/// the reset pin of [`Ch347Config`]. Without either, the driver writes no GPIO.
#[derive(Debug)]
pub struct Ch347 {
    device: Ch347Device,
    jtag_state: JtagChainState,
}

/// Driver settings of a CH347.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ch347Config {
    /// The pin wired to the target's reset line. Replaces the reset line of a known board.
    pub reset_pin: Option<Ch347ResetPin>,
}

/// A CH347 pin that drives the target's reset line. The line is released to an input, so it
/// needs a pull-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ch347ResetPin {
    /// The TRST pin. JTAG drives this pin, so this forces SWD.
    Trst,
    /// A GPIO, 0 to 7. Check the schematic of the board: the GPIO must carry no other signal.
    Gpio(u8),
}

impl FromStr for Ch347ResetPin {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let pin = s.to_ascii_lowercase();
        if pin == "trst" {
            return Ok(Self::Trst);
        }
        pin.strip_prefix("gpio")
            .and_then(|gpio| match gpio.as_bytes() {
                [digit @ b'0'..=b'7'] => Some(digit - b'0'),
                _ => None,
            })
            .map(Self::Gpio)
            .ok_or_else(|| {
                format!("invalid reset pin `{s}`, expected `trst` or `gpio0` to `gpio7`")
            })
    }
}

impl std::fmt::Display for Ch347ResetPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trst => f.write_str("trst"),
            Self::Gpio(gpio) => write!(f, "gpio{gpio}"),
        }
    }
}

impl Serialize for Ch347ResetPin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Ch347ResetPin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
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
    /// Applies the driver settings. Call this before attach.
    pub fn configure(&mut self, config: &Ch347Config) -> Result<(), DebugProbeError> {
        if let Some(pin) = config.reset_pin {
            self.device.set_reset_pin(pin)?;
        }
        Ok(())
    }

    fn jtag(&self) -> bool {
        self.device.protocol() == WireProtocol::Jtag
    }

    fn no_jtag() -> DebugProbeError {
        DebugProbeError::InterfaceNotAvailable {
            interface_name: "JTAG",
        }
    }
}

impl JtagProbe for Ch347 {
    fn run_batch(
        &mut self,
        batch: &JtagBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        self.device
            .run_jtag_batch(&mut self.jtag_state.tap_state, batch)
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
        if self.jtag() {
            self.device.run_swj_batch(batch)
        } else {
            self.device.run_swd_batch(batch)
        }
    }

    /// The engine owns WAIT: a replay from the WAITed access would repeat those after it.
    fn handles_wait(&self) -> bool {
        true
    }

    /// Every read gets its own value, so the host adds no RDBUFF reads.
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
        self.device.attach()?;
        self.device.set_led(true)
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(self.device.set_led(false)?)
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.device.target_reset()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.device.target_reset_assert()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.device.target_reset_deassert()
    }

    fn set_reset_line_connected(&mut self, connected: bool) {
        self.device.reset_line.set_connected(connected);
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
        Some(self)
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
