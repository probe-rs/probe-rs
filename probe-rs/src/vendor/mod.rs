//! Vendor support modules.

use std::{
    ops::Deref,
    sync::{Arc, LazyLock},
};

use parking_lot::{RecursiveRwLock, RecursiveRwLockReadGuard};
use probe_rs_target::Chip;

use crate::{
    Error, Target,
    architecture::{
        arm::{
            ArmChipInfo, ArmDebugInterface,
            communication_interface::read_chip_info_from_rom_table,
            dp::DpAddress,
            sequences::{ArmDebugSequence, DefaultArmSequence},
        },
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState,
        },
    },
    config::{ChipInfo, DebugSequence, Registry},
    probe::Probe,
};

pub mod amd;
pub mod asrmicro;
pub mod gigadevice;
pub mod holtek;
pub mod infineon;
pub mod maxim;
pub mod microchip;
pub mod nordicsemi;
pub mod nsing;
pub mod nuclei;
pub mod nxp;
pub mod raspberrypi;
pub mod renesas;
pub mod sifive;
pub mod sifli;
pub mod silabs;
pub mod st;
pub mod ti;
pub mod tridentiot;
pub mod vorago;
pub mod wch;
pub mod whxy;

/// Vendor support trait.
pub trait Vendor: Send + Sync + std::fmt::Display {
    /// Tries to create a debug sequence for the given chip.
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence>;

    /// Tries to identify an ARM chip. Returns `Some(target name)` on success.
    fn try_detect_arm_chip(
        &self,
        _registry: &Registry,
        _probe: &mut dyn ArmDebugInterface,
        _chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to find an ARM chip that the default sequence can't reach, e.g. behind a JTAG
    /// router. Returns `Some(sequence)` that reaches it, for the ARM chip detection.
    fn try_create_arm_detection_sequence(
        &self,
        _registry: &Registry,
        _probe: &mut Probe,
    ) -> Result<Option<Arc<dyn ArmDebugSequence>>, Error> {
        Ok(None)
    }

    /// Tries to identify an ARM chip reached by the sequence from
    /// [`Vendor::try_create_arm_detection_sequence`]. Returns `Some(target name)` on success.
    fn try_identify_arm_chip(
        &self,
        _registry: &Registry,
        _interface: &mut dyn ArmDebugInterface,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to identify a RISC-V chip. Returns `Some(target name)` on success.
    fn try_detect_riscv_chip(
        &self,
        _registry: &Registry,
        _probe: &mut RiscvCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to identify an Xtensa chip. Returns `Some(target name)` on success.
    fn try_detect_xtensa_chip(
        &self,
        _registry: &Registry,
        _probe: &mut XtensaCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to identify a chip from probe-firmware-side metadata. Returns `Some(target name)` on success.
    fn try_detect_chip_from_probe(
        &self,
        _registry: &Registry,
        _probe: &mut Probe,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }
}

static VENDORS: LazyLock<RecursiveRwLock<Vec<&'static dyn Vendor>>> = LazyLock::new(|| {
    let vendors: Vec<&'static dyn Vendor> = vec![
        &asrmicro::Asrmicro,
        &amd::Amd,
        &microchip::Microchip,
        &infineon::Infineon,
        &maxim::Maxim,
        &holtek::Holtek,
        &silabs::SiliconLabs,
        &ti::TexasInstruments,
        &nordicsemi::NordicSemi,
        &nuclei::Nuclei,
        &nxp::Nxp,
        &raspberrypi::RaspberryPi,
        &st::St,
        &vorago::Vorago,
        &sifive::Sifive,
        &sifli::Sifli,
        &renesas::Renesas,
        &wch::Wch,
        &gigadevice::GigaDevice,
        &nsing::Nsing,
        &whxy::Whxy,
        &tridentiot::TridentIot,
    ];

    RecursiveRwLock::new(vendors)
});

/// Registers a new vendor.
pub(crate) fn register_vendor(vendor: &'static dyn Vendor) {
    // Order matters. Prepend to allow users to override the default vendors.
    VENDORS.write().insert(0, vendor);
}

/// Returns a readable view of all known vendors.
fn vendors<'a>() -> impl Deref<Target = [&'a dyn Vendor]> {
    RecursiveRwLockReadGuard::map(VENDORS.read(), |v| v.as_slice())
}

/// Tries to create a debug sequence for the given chip.
pub fn try_create_debug_sequence(chip: &Chip) -> Option<DebugSequence> {
    let vendors = vendors();
    for vendor in vendors.iter() {
        if let Some(sequence) = vendor.try_create_debug_sequence(chip) {
            return Some(sequence);
        }
    }

    None
}

fn try_detect_arm_chip(
    registry: &Registry,
    mut probe: Probe,
) -> (Probe, Result<Option<Target>, Error>) {
    if !probe.has_arm_debug_interface() {
        // No ARM interface available.
        tracing::debug!("No ARM interface available, skipping detection.");
        return (probe, Ok(None));
    }

    // We don't know what kind of chip it is, so we use the default sequence, unless a vendor
    // finds a chip that the default sequence can't reach.
    let mut sequence = DefaultArmSequence::create();
    let mut sequence_vendor = None;
    for vendor in vendors().iter() {
        match vendor.try_create_arm_detection_sequence(registry, &mut probe) {
            Ok(Some(vendor_sequence)) => {
                tracing::debug!("Using the {vendor} sequence for detection");
                sequence = vendor_sequence;
                sequence_vendor = Some(*vendor);
                break;
            }
            Ok(None) => {}
            Err(error) => tracing::debug!("{vendor} sequence detection failed: {error}"),
        }
    }

    // We have no information about the target, so we must assume it's using the default DP.
    // We cannot automatically detect DPs if SWD multi-drop is used.
    // Most multi-drop implementations will expose enough info via the default DP to do detection
    // so at least when there's only one SWD target, DpAddress::Default works fine.
    // RP2040 isn't one of those - it's only accessible via multi-drop, so try it after Default.
    let dp_addresses = [
        DpAddress::Default,
        DpAddress::Multidrop(0x01002927), // RP2040 core0
    ];

    for dp_address in dp_addresses {
        // TODO: do not consume probe
        match probe.try_into_arm_debug_interface(sequence.clone()) {
            Ok(mut interface) => {
                let found =
                    identify_arm_chip(registry, interface.as_mut(), dp_address, sequence_vendor);
                probe = interface.close();
                match found {
                    Ok(None) => {}
                    found => return (probe, found),
                }
            }
            Err((returned_probe, error)) => {
                probe = returned_probe;
                tracing::debug!("Error using ARM interface: {error}");
            }
        }
    }

    (probe, Ok(None))
}

/// Identifies the ARM chip behind `dp_address`. `Ok(None)` if nothing answers there or its ROM
/// table can't be read.
fn identify_arm_chip(
    registry: &Registry,
    interface: &mut dyn ArmDebugInterface,
    dp_address: DpAddress,
    sequence_vendor: Option<&dyn Vendor>,
) -> Result<Option<Target>, Error> {
    if let Err(error) = interface.select_debug_port(dp_address) {
        tracing::debug!("Error during ARM chip detection on {dp_address:?}: {error}");
        return Ok(None);
    }

    if let Some(vendor) = sequence_vendor {
        match vendor.try_identify_arm_chip(registry, interface) {
            Ok(Some(target_name)) => return Ok(Some(registry.get_target_by_name(&target_name)?)),
            Ok(None) => {}
            Err(error) => tracing::debug!("{vendor} chip detection failed: {error}"),
        }
    }

    let found_arm_chip =
        read_chip_info_from_rom_table(interface, dp_address).unwrap_or_else(|error| {
            tracing::debug!("Error during ARM chip detection: {error}");
            None
        });
    let Some(found_chip) = found_arm_chip else {
        return Ok(None);
    };

    let vendors = vendors();
    for vendor in vendors.iter() {
        // TODO: only consider families with matching JEP106.
        if let Some(target_name) = vendor.try_detect_arm_chip(registry, interface, found_chip)? {
            return Ok(Some(registry.get_target_by_name(&target_name)?));
        }
    }

    // No vendor-specific match, try to find a target by chip info.
    Ok(Some(
        registry.get_target_by_chip_info(ChipInfo::from(found_chip))?,
    ))
}

fn try_detect_riscv_chip(registry: &Registry, probe: &mut Probe) -> Result<Option<Target>, Error> {
    let mut found_target = None;

    if !probe.has_riscv_interface() {
        tracing::debug!("No RISC-V interface available, skipping detection.");
        return Ok(None);
    }

    if let Some(mut chain) = probe.try_as_jtag_chain() {
        _ = chain.select(0);
    }

    match probe.try_get_riscv_interface_builder() {
        Ok(factory) => {
            let mut state = factory.create_state();
            let mut interface = factory.attach(&mut state)?;

            if let Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter RISC-V debug mode: {error}");
                return Ok(None);
            }

            match interface.read_idcode() {
                Ok(Some(idcode)) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        // TODO: only consider families with matching JEP106.
                        if let Some(target_name) =
                            vendor.try_detect_riscv_chip(registry, &mut interface, idcode)?
                        {
                            found_target = Some(registry.get_target_by_name(target_name)?);
                            break;
                        }
                    }
                }
                Ok(_) => tracing::debug!("No RISC-V ID code returned."),
                Err(error) => tracing::debug!("Error during RISC-V chip detection: {error}"),
            }

            // TODO: disable debug module
        }

        Err(error) => {
            tracing::debug!("Error during RISC-V chip detection: {error}");
        }
    }

    Ok(found_target)
}

fn try_detect_xtensa_chip(registry: &Registry, probe: &mut Probe) -> Result<Option<Target>, Error> {
    let mut found_target = None;

    if !probe.has_xtensa_interface() {
        tracing::debug!("No Xtensa interface available, skipping detection.");
        return Ok(None);
    }

    if let Some(mut chain) = probe.try_as_jtag_chain() {
        _ = chain.select(0);
    }

    let mut state = XtensaDebugInterfaceState::default();
    match probe.try_get_xtensa_interface(&mut state) {
        Ok(mut interface) => {
            if let Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter Xtensa debug mode: {error}");
                return Ok(None);
            }

            match interface.read_idcode() {
                Ok(Some(idcode)) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        // TODO: only consider families with matching JEP106.
                        if let Some(target_name) =
                            vendor.try_detect_xtensa_chip(registry, &mut interface, idcode)?
                        {
                            found_target = Some(registry.get_target_by_name(target_name)?);
                            break;
                        }
                    }
                }
                Ok(_) => tracing::debug!("No Xtensa ID code returned."),
                Err(error) => tracing::debug!("Error during Xtensa chip detection: {error}"),
            }

            interface.leave_debug_mode()?;
        }

        Err(error) => {
            tracing::debug!("Error during autodetection of Xtensa chips: {error}");
        }
    }

    Ok(found_target)
}

fn try_detect_from_probe(registry: &Registry, probe: &mut Probe) -> Result<Option<Target>, Error> {
    for vendor in vendors().iter() {
        if let Some(target_name) = vendor.try_detect_chip_from_probe(registry, probe)? {
            return Ok(Some(registry.get_target_by_name(target_name)?));
        }
    }
    Ok(None)
}

/// Tries to identify the chip using the given probe. The probe comes back whatever the result.
pub(crate) fn auto_determine_target(
    registry: &Registry,
    mut probe: Probe,
) -> (Probe, Result<Option<Target>, Error>) {
    tracing::info!("Auto-detecting target");
    let mut found_target = None;

    // Xtensa and RISC-V interfaces don't need moving the probe. For clarity, their
    // handlers work with the borrowed probe, and we use these wrappers to adapt to the
    // ARM way of moving in and out of the probe.
    //
    // Detection is a best-effort operation (see comment on `TargetSelector::Auto` above), so
    // an error from one architecture's detection logic (e.g. a probe/board combination that
    // physically cannot perform JTAG signaling, like an SWD-only connection) must not abort
    // the whole auto-detection sequence. The probe itself is only borrowed by the inner
    // functions, so it's still valid to keep using after an error and try the next
    // architecture.
    fn try_detect_from_probe_wrapper(
        registry: &Registry,
        mut probe: Probe,
    ) -> (Probe, Result<Option<Target>, Error>) {
        let found_target = try_detect_from_probe(registry, &mut probe).unwrap_or_else(|error| {
            tracing::debug!("Error during chip auto-detection: {error}");
            None
        });
        (probe, Ok(found_target))
    }

    fn try_detect_riscv_chip_wrapper(
        registry: &Registry,
        mut probe: Probe,
    ) -> (Probe, Result<Option<Target>, Error>) {
        let found_target = try_detect_riscv_chip(registry, &mut probe).unwrap_or_else(|error| {
            tracing::debug!("Error during RISC-V chip auto-detection: {error}");
            None
        });
        (probe, Ok(found_target))
    }

    fn try_detect_xtensa_chip_wrapper(
        registry: &Registry,
        mut probe: Probe,
    ) -> (Probe, Result<Option<Target>, Error>) {
        let found_target = try_detect_xtensa_chip(registry, &mut probe).unwrap_or_else(|error| {
            tracing::debug!("Error during Xtensa chip auto-detection: {error}");
            None
        });
        (probe, Ok(found_target))
    }

    type DetectFn = fn(&Registry, Probe) -> (Probe, Result<Option<Target>, Error>);
    const ARCHITECTURES: &[DetectFn] = &[
        try_detect_from_probe_wrapper,
        try_detect_arm_chip,
        try_detect_riscv_chip_wrapper,
        try_detect_xtensa_chip_wrapper,
    ];

    for architecture in ARCHITECTURES {
        let (returned_probe, target) = architecture(registry, probe);
        probe = returned_probe;
        match target {
            Ok(Some(target)) => {
                tracing::info!("Found target: {}", target.name);
                found_target = Some(target);
                break;
            }
            Ok(None) => {}
            Err(error) => return (probe, Err(error)),
        }
    }

    match probe.detach() {
        Ok(()) => (probe, Ok(found_target)),
        Err(error) => (probe, Err(error)),
    }
}
