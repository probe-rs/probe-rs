use super::{Core, MemoryRegion, RawFlashAlgorithm, TargetDescriptionSource};
use crate::{
    architecture::{
        arm::{
            ApV2Address, FullyQualifiedApAddress,
            dp::DpAddress,
            sequences::{ArmDebugSequence, DefaultArmSequence},
        },
        riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence},
        xtensa::sequences::{DefaultXtensaSequence, XtensaDebugSequence},
    },
    flashing::{DebugFlashSequence, FlashVerify},
    rtt::ScanRegion,
};
use probe_rs_target::{
    Architecture, Chip, ChipFamily, CoreAccessOptions, Jtag, MemoryAccess, MemoryPort,
    MemoryRange as _, NvmRegion,
};
use std::sync::Arc;

/// This describes a complete target with a fixed chip model and variant.
#[derive(Clone)]
pub struct Target {
    /// The name of the target.
    pub name: String,
    /// The cores of the target.
    pub cores: Vec<Core>,
    /// Additional memory access ports of the target.
    pub memory_ports: Vec<MemoryPort>,
    /// The list of available flash algorithms.
    pub flash_algorithms: Vec<RawFlashAlgorithm>,
    /// The memory map of the target.
    pub memory_map: Vec<MemoryRegion>,
    /// Source of the target description. Used for diagnostics.
    pub(crate) source: TargetDescriptionSource,
    /// Debug sequences for the given target.
    pub debug_sequence: DebugSequence,
    /// The regions of memory to scan to try to find an RTT header.
    ///
    /// Each region must be enclosed in exactly one RAM region from
    /// `memory_map`.
    pub rtt_scan_regions: ScanRegion,
    /// The Description of the scan chain
    ///
    /// The scan chain can be parsed from the CMSIS-SDF file, or specified
    /// manually in the target.yaml file. It is used by some probes to determine
    /// the number devices in the scan chain and their ir lengths.
    pub jtag: Option<Jtag>,
    /// The default executable format for the target.
    pub default_format: Option<String>,
    /// Skip the reset that normally precedes RAM flashing when booting from RAM. See
    /// [`Chip::skip_reset_on_ram_boot`](probe_rs_target::Chip::skip_reset_on_ram_boot).
    pub skip_reset_on_ram_boot: bool,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Target {{
            identifier: {:?},
            flash_algorithms: {:?},
            memory_map: {:?},
        }}",
            self.name, self.flash_algorithms, self.memory_map
        )
    }
}

impl Target {
    /// Create a new target for the given details.
    ///
    /// The given chip must be a member of the given family.
    pub(super) fn new(family: &ChipFamily, chip: &Chip) -> Target {
        let mut memory_map = chip.memory_map.clone();
        let mut flash_algorithms = Vec::new();
        for algo_name in chip.flash_algorithms.iter() {
            let Some(algo) = family.get_algorithm_for_chip(algo_name, chip) else {
                unreachable!(
                    "The chip {chip_name} refers to a flash algorithm called {algo_name}, which is \
                    not defined in the family {family_name}. The flash algorithms should have been \
                    validated when the ChipFamily was loaded. This is a bug, please report it.",
                    chip_name = chip.name,
                    family_name = family.name,
                );
            };

            // If the flash algorithm addresses a range that is not covered by any memory region,
            // we add a new memory region for it. This is usually the case for option bytes and
            // some OTP memory regions in certain CMSIS packs.
            let algo_range = &algo.flash_properties.address_range;
            if !memory_map
                .iter()
                .any(|region| region.address_range().intersects_range(algo_range))
            {
                // HACK (doubly, even):
                // Conjure up a memory region for the flash algorithm. This is mostly used by
                // EEPROM regions. We probably don't want to erase these regions, so we set
                // `is_alias`, which then causes "erase all" to skip the region.
                memory_map.push(MemoryRegion::Nvm(NvmRegion {
                    name: Some(format!("synthesized for {algo_name}")),
                    range: algo_range.clone(),
                    cores: if algo.cores.is_empty() {
                        chip.cores.iter().map(|core| core.name.clone()).collect()
                    } else {
                        algo.cores.clone()
                    },
                    is_alias: true,
                    access: Some(MemoryAccess {
                        read: false,
                        write: false,
                        execute: false,
                        boot: false,
                    }),
                    memory_ports: vec![],
                }));
            }

            flash_algorithms.push(algo);
        }

        let debug_sequence = crate::vendor::try_create_debug_sequence(chip).unwrap_or_else(|| {
            // Default to the architecture of the first core, which is okay if
            // there is no mixed architectures.
            match chip.cores[0].core_type.architecture() {
                Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::create()),
                Architecture::Riscv => DebugSequence::Riscv(DefaultRiscvSequence::create()),
                Architecture::Xtensa => DebugSequence::Xtensa(DefaultXtensaSequence::create()),
            }
        });

        tracing::info!("Using sequence {:?}", debug_sequence);

        let rtt_scan_regions = match &chip.rtt_scan_ranges {
            Some(ranges) => ScanRegion::Ranges(ranges.clone()),
            None => ScanRegion::Ram, // By default we use all of the RAM ranges from the memory map.
        };

        Target {
            name: chip.name.clone(),
            cores: chip.cores.clone(),
            memory_ports: chip.memory_ports.clone(),
            flash_algorithms,
            source: family.source.clone(),
            memory_map,
            debug_sequence,
            rtt_scan_regions,
            jtag: chip.jtag.clone(),
            default_format: chip.default_binary_format.clone(),
            skip_reset_on_ram_boot: chip.skip_reset_on_ram_boot,
        }
    }

    /// Get the architecture of the target
    pub fn architecture(&self) -> Architecture {
        let target_arch = self.cores[0].core_type.architecture();

        // This should be ensured when a `ChipFamily` is loaded.
        assert!(
            self.cores
                .iter()
                .map(|core| core.core_type.architecture())
                .all(|core_arch| core_arch == target_arch),
            "Not all cores of the target are of the same architecture. Probe-rs doesn't support this (yet). If you see this, it is a bug. Please file an issue."
        );

        target_arch
    }

    /// Return the default core of the target, usually the first core.
    ///
    /// This core should be used for operations such as debug_unlock,
    /// when nothing else is specified.
    pub fn default_core(&self) -> &Core {
        // TODO: Check if this is specified in the target description.
        &self.cores[0]
    }

    /// Returns whether a session reaches the target through an ARM debug port. Only these
    /// targets drive the reset line when attaching under reset.
    pub fn uses_arm_debug_interface(&self) -> bool {
        self.default_core().memory_ap().is_some()
    }

    /// Returns whether the described JTAG scan chain has a TAP for any core of the target.
    ///
    /// A target that describes its scan chain without a TAP at any core's `jtag_tap` index can't
    /// be debugged over JTAG, for example because its JTAG pins only reach a boundary-scan TAP.
    /// A target without a described scan chain is assumed to be reachable.
    pub(crate) fn jtag_reaches_any_core(&self) -> bool {
        let Some(scan_chain) = self.jtag.as_ref().and_then(|jtag| jtag.scan_chain.as_ref()) else {
            return true;
        };
        self.cores.iter().any(|core| {
            let jtag_tap = match &core.core_access_options {
                CoreAccessOptions::Arm(options) => options.jtag_tap,
                CoreAccessOptions::Riscv(options) => options.jtag_tap,
                CoreAccessOptions::Xtensa(options) => options.jtag_tap,
            };
            jtag_tap.unwrap_or(0) < scan_chain.len()
        })
    }

    /// Source description of this target.
    pub fn source(&self) -> &TargetDescriptionSource {
        &self.source
    }

    /// Create a [FlashLoader](crate::flashing::FlashLoader) for this target, which can be used
    /// to program its non-volatile memory.
    pub fn flash_loader(&self) -> crate::flashing::FlashLoader {
        crate::flashing::FlashLoader::new(self.memory_map.clone(), self.source.clone())
    }

    /// Returns a [RawFlashAlgorithm] by name.
    pub(crate) fn flash_algorithm_by_name(&self, name: &str) -> Option<&RawFlashAlgorithm> {
        self.flash_algorithms.iter().find(|a| a.name == name)
    }

    /// Returns the core index from the core name
    pub fn core_index_by_name(&self, name: &str) -> Option<usize> {
        self.cores.iter().position(|c| c.name == name)
    }

    /// Returns the core index from the core name
    pub fn core_index_by_address(&self, address: u64) -> Option<usize> {
        let target_memory = self.memory_region_by_address(address)?;
        let core_name = target_memory.cores().first()?;
        self.core_index_by_name(core_name)
    }

    /// Returns the first found [MemoryRegion] that contains the given address
    pub fn memory_region_by_address(&self, address: u64) -> Option<&MemoryRegion> {
        self.memory_map
            .iter()
            .find(|region| region.contains(address))
    }
}

/// Selector for the debug target.
#[derive(Debug, Clone)]
pub enum TargetSelector {
    /// Specify the name of a target, which will
    /// be used to search the internal list of
    /// targets.
    Unspecified(String),
    /// Directly specify a target. Boxed, because the target structure has become quite large.
    Specified(Box<Target>),
    /// Try to automatically identify the target,
    /// by reading identifying information from
    /// the probe and / or target.
    Auto,
}

impl From<&str> for TargetSelector {
    fn from(value: &str) -> Self {
        TargetSelector::Unspecified(value.into())
    }
}

impl From<&String> for TargetSelector {
    fn from(value: &String) -> Self {
        TargetSelector::Unspecified(value.into())
    }
}

impl From<String> for TargetSelector {
    fn from(value: String) -> Self {
        TargetSelector::Unspecified(value)
    }
}

impl From<Option<&str>> for TargetSelector {
    fn from(value: Option<&str>) -> Self {
        match value {
            Some(identifier) => identifier.into(),
            None => TargetSelector::Auto,
        }
    }
}

impl From<()> for TargetSelector {
    fn from(_value: ()) -> Self {
        TargetSelector::Auto
    }
}

impl From<Target> for TargetSelector {
    fn from(target: Target) -> Self {
        TargetSelector::Specified(Box::new(target))
    }
}

/// This is the type to denote a general debug sequence.
/// It can differentiate between ARM, RISC-V and Xtensa for now.
#[derive(Clone, Debug)]
pub enum DebugSequence {
    /// An ARM debug sequence.
    Arm(Arc<dyn ArmDebugSequence>),
    /// A RISC-V debug sequence.
    Riscv(Arc<dyn RiscvDebugSequence>),
    /// An Xtensa debug sequence.
    Xtensa(Arc<dyn XtensaDebugSequence>),
}

impl DebugSequence {
    /// Return the host-side flash sequence for this target, if one is registered.
    ///
    /// This delegates to the architecture-specific sequence method, providing a
    /// single architecture-agnostic entry point for the flash loader.
    pub fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        match self {
            DebugSequence::Arm(seq) => seq.debug_flash_sequence(),
            DebugSequence::Riscv(seq) => seq.debug_flash_sequence(),
            DebugSequence::Xtensa(seq) => seq.debug_flash_sequence(),
        }
    }

    /// Return the fast flash verification implementation for this target, if one is
    /// registered.
    ///
    /// Like [`Self::debug_flash_sequence`], this gives the flash loader one
    /// architecture-agnostic entry point.
    pub fn flash_verify_sequence(&self) -> Option<Arc<dyn FlashVerify>> {
        match self {
            DebugSequence::Arm(seq) => seq.flash_verify_sequence(),
            DebugSequence::Riscv(seq) => seq.flash_verify_sequence(),
            DebugSequence::Xtensa(seq) => seq.flash_verify_sequence(),
        }
    }
}

pub(crate) trait MemoryApExt {
    /// Retrieve the Coresight MemoryAP through which this core or
    /// memory port is accessed, if available.
    fn memory_ap(&self) -> Option<FullyQualifiedApAddress>;
}

fn fully_qualified_ap(
    targetsel: Option<u32>,
    ap: &probe_rs_target::ApAddress,
) -> FullyQualifiedApAddress {
    let dp = match targetsel {
        None => DpAddress::Default,
        Some(x) => DpAddress::Multidrop(x),
    };
    match ap {
        probe_rs_target::ApAddress::V1(ap) => FullyQualifiedApAddress::v1_with_dp(dp, *ap),
        probe_rs_target::ApAddress::V2(ap) => {
            FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(*ap))
        }
    }
}

impl MemoryApExt for Core {
    fn memory_ap(&self) -> Option<FullyQualifiedApAddress> {
        match &self.core_access_options {
            probe_rs_target::CoreAccessOptions::Arm(options) => {
                Some(fully_qualified_ap(options.targetsel, &options.ap))
            }
            probe_rs_target::CoreAccessOptions::Riscv(options) => options
                .mem_ap
                .as_ref()
                .map(|ap| fully_qualified_ap(None, ap)),
            probe_rs_target::CoreAccessOptions::Xtensa(_) => None,
        }
    }
}

impl MemoryApExt for MemoryPort {
    fn memory_ap(&self) -> Option<FullyQualifiedApAddress> {
        match &self.memory_port_options {
            probe_rs_target::MemoryPortOptions::Arm(options) => {
                Some(fully_qualified_ap(options.targetsel, &options.ap))
            }
        }
    }
}

#[cfg(all(test, feature = "builtin-targets"))]
mod tests {
    use crate::config::{Registry, Target};
    use probe_rs_target::CoreAccessOptions;

    fn target(name: &str) -> Target {
        Registry::from_builtin_families()
            .get_target_by_name(name)
            .unwrap()
    }

    #[test]
    fn jtag_reaches_any_core() {
        // No described scan chain.
        assert!(target("nRF52840_xxAA").jtag_reaches_any_core());
        // The core TAP is the second of two.
        assert!(target("X7Z").jtag_reaches_any_core());
        // The core TAP is the first of two.
        assert!(target("CC2652R").jtag_reaches_any_core());
        // The JTAG pins only reach a boundary-scan TAP.
        assert!(!target("S32K118xxxxVLHx").jtag_reaches_any_core());
    }

    #[test]
    fn jtag_reaches_one_of_two_cores() {
        let mut zynq = target("X7Z");
        let CoreAccessOptions::Arm(options) = &mut zynq.cores[1].core_access_options else {
            panic!("X7Z has ARM cores");
        };
        options.jtag_tap = Some(2);
        assert!(zynq.jtag_reaches_any_core());
    }
}
