use anyhow::{anyhow, bail, Context, Error, Result};
use cmsis_pack::pdsc::{AccessPort, Algorithm, Core, Device, Package, Processor};
use cmsis_pack::{pack_index::PdscRef, utils::FromElem};
use futures::StreamExt;
use probe_rs::flashing::FlashAlgorithm;
use probe_rs_target::{
    Architecture, ArmCoreAccessOptions, Chip, ChipFamily, Core as ProbeCore, CoreAccessOptions,
    CoreType, GenericRegion, MemoryAccess, MemoryRange as _, MemoryRegion, NvmRegion, RamRegion,
    RawFlashAlgorithm, RiscvCoreAccessOptions, TargetDescriptionSource, XtensaCoreAccessOptions,
};
use std::collections::HashMap;
use std::{fs, io::Read, path::Path};

pub enum Kind<'a, T>
where
    T: std::io::Seek + std::io::Read,
{
    Archive(&'a mut zip::ZipArchive<T>),
    Directory(&'a Path),
}

impl<'a, T> Kind<'a, T>
where
    T: std::io::Seek + std::io::Read,
{
    /// Read binary data from the given path.
    fn read_bytes(&mut self, path: &Path) -> Result<Vec<u8>> {
        let buffer = match self {
            Kind::Archive(archive) => {
                let reader = archive.by_name(&path.to_string_lossy())?;
                reader.bytes().collect::<std::io::Result<Vec<u8>>>()?
            }
            Kind::Directory(dir) => fs::read(dir.join(path))?,
        };

        Ok(buffer)
    }
}

fn process_flash_algo<T>(
    flash_algorithm: &Algorithm,
    kind: &mut Kind<T>,
) -> Result<RawFlashAlgorithm>
where
    T: std::io::Seek + std::io::Read,
{
    let algo_bytes = kind.read_bytes(&flash_algorithm.file_name)?;
    let mut algo = crate::parser::extract_flash_algo(
        &algo_bytes,
        &flash_algorithm.file_name,
        flash_algorithm.default,
        false, // Algorithms from CMSIS-Pack files are position independent
    )?;

    // If the algo specifies `RAMstart` and/or `RAMsize` fields, then use them.
    // - See https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/pdsc_family_pg.html#element_algorithm for more information.
    algo.load_address = flash_algorithm
        .ram_start
        .map(|ram_start| ram_start + FlashAlgorithm::get_max_algorithm_header_size());

    // This algo will still be added to the specific chip algos by name.
    // We just need to deduplicate the entire flash algorithm and reference to it by name at other places.

    Ok(algo)
}

pub(crate) fn extract_families<T>(
    pdsc: Package,
    mut kind: Kind<T>,
    families: &mut Vec<ChipFamily>,
    only_supported_familes: bool,
) -> Result<()>
where
    T: std::io::Seek + std::io::Read,
{
    // Forge a definition file for each device in the .pdsc file.
    let mut devices = pdsc.devices.0.into_iter().collect::<Vec<_>>();
    devices.sort_by(|a, b| a.0.cmp(&b.0));

    // Only process this, if this belongs to a supported family.
    let currently_supported_chip_families = probe_rs::config::families();

    for (device_name, device) in devices {
        if only_supported_familes
            && !currently_supported_chip_families
                .iter()
                .any(|supported_family| supported_family.name == device.family)
        {
            // We only want to continue if the chip family is already represented as supported probe_rs target chip family.
            log::debug!("Unsupprted chip family {}. Skipping ...", device.family);
            break;
        }

        // Check if this device family is already known.
        let mut potential_family = families
            .iter_mut()
            .find(|family| family.name == device.family);

        let family = if let Some(ref mut family) = potential_family {
            family
        } else {
            families.push(ChipFamily {
                name: device.family.clone(),
                manufacturer: None,
                generated_from_pack: true,
                chip_detection: vec![],
                pack_file_release: Some(pdsc.releases.latest_release().version.clone()),
                variants: Vec::new(),
                flash_algorithms: Vec::new(),
                source: TargetDescriptionSource::BuiltIn,
            });
            // This unwrap is always safe as we insert at least one item previously.
            families.last_mut().unwrap()
        };

        let cores = device
            .processors
            .iter()
            .map(create_core)
            .collect::<Result<Vec<_>>>()?;

        let mut memory_map = get_mem_map(&device, &cores);
        patch_memmap(&mut memory_map);

        // Extract the flash algorithm, block & sector size and the erased byte value from the ELF binary.
        let flash_algorithm_names = device
            .algorithms
            .iter()
            .filter_map(|flash_algorithm| {
                match process_flash_algo(flash_algorithm, &mut kind) {
                    Ok(algo) => {
                        let address_range = &algo.flash_properties.address_range;

                        // Only add the algo to the device if it targets a memory region that the
                        // device has.
                        if !memory_map
                            .iter()
                            .any(|region| region.address_range().contains_range(address_range))
                        {
                            return None;
                        }

                        let algo_name = algo.name.clone();
                        // We add this algo directly to the algos of the family if it's not already added.
                        // Make sure we never add an algo twice to save file size.
                        if !family.flash_algorithms.contains(&algo) {
                            family.flash_algorithms.push(algo);
                        }
                        Some(algo_name)
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to process flash algorithm {}.",
                            flash_algorithm.file_name.display()
                        );
                        log::warn!("Reason: {:?}", e);
                        None
                    }
                }
            })
            .collect::<Vec<_>>();

        // Sometimes the algos are referenced twice, for example in the multicore H7s
        // Deduplicate while keeping order.
        let flash_algorithm_names = flash_algorithm_names
            .iter()
            .enumerate()
            .filter(|(i, s)| !flash_algorithm_names[..*i].contains(s))
            .map(|(_, s)| s.clone())
            .collect::<Vec<_>>();

        family.variants.push(Chip {
            name: device_name,
            part: None,
            svd: None,
            documentation: HashMap::new(),
            cores,
            memory_map,
            flash_algorithms: flash_algorithm_names,
            rtt_scan_ranges: None,
            jtag: None, // TODO, parse scan chain from sdf
            default_binary_format: None,
        });
    }

    Ok(())
}

fn create_core(processor: &Processor) -> Result<ProbeCore> {
    let core_type = core_to_probe_core(&processor.core)?;
    Ok(ProbeCore {
        name: processor
            .name
            .as_ref()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "main".to_string()),
        core_type,
        core_access_options: match core_type.architecture() {
            Architecture::Arm => CoreAccessOptions::Arm(ArmCoreAccessOptions {
                ap: match processor.ap {
                    AccessPort::Index(id) => id,
                    AccessPort::Address(_) => todo!(),
                },
                psel: 0,
                debug_base: None,
                cti_base: None,
            }),
            Architecture::Riscv => CoreAccessOptions::Riscv(RiscvCoreAccessOptions {
                hart_id: None,
                jtag_tap: None,
            }),
            Architecture::Xtensa => {
                CoreAccessOptions::Xtensa(XtensaCoreAccessOptions { jtag_tap: None })
            }
        },
    })
}

fn core_to_probe_core(value: &Core) -> Result<CoreType, Error> {
    Ok(match value {
        Core::CortexM0 => CoreType::Armv6m,
        Core::CortexM0Plus => CoreType::Armv6m,
        Core::CortexM4 => CoreType::Armv7em,
        Core::CortexM3 => CoreType::Armv7m,
        Core::CortexM23 => CoreType::Armv8m,
        Core::CortexM33 => CoreType::Armv8m,
        Core::CortexM55 => CoreType::Armv8m,
        Core::CortexM85 => CoreType::Armv8m,
        Core::CortexM7 => CoreType::Armv7em,
        Core::StarMC1 => CoreType::Armv8m,
        c => bail!("Core '{c:?}' is not yet supported for target generation."),
    })
}

// Process all `.pdsc` files in the given directory.
pub fn visit_dirs(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    walk_files(path, &mut |path| {
        if has_extension(path, "pack") {
            log::info!("Found .pdsc file: {}", path.display());

            let package = Package::from_path(path)
                .context(format!("Failed to open .pdsc file {}.", path.display()))?;

            extract_families::<fs::File>(package, Kind::Directory(path), families, false)
                .context(format!("Failed to process .pdsc file {}.", path.display()))?;
        }

        Ok(())
    })
}

fn walk_files(path: &Path, callback: &mut impl FnMut(&Path) -> Result<()>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry_path = entry?.path();

        if entry_path.is_dir() {
            walk_files(&entry_path, callback)?;
        } else {
            callback(&entry_path)?;
        }
    }

    Ok(())
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().map_or(false, |e| e == ext)
}

pub fn visit_file(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    log::info!("Trying to open pack file: {}.", path.display());
    // If we get a file, try to unpack it.
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    let mut pdsc_file = find_pdsc_in_archive(&mut archive)?
        .ok_or_else(|| anyhow!("Failed to find .pdsc file in archive {}", path.display()))?;

    let mut pdsc = String::new();
    pdsc_file.read_to_string(&mut pdsc)?;

    let package = Package::from_string(&pdsc).map_err(|e| {
        anyhow!(
            "Failed to parse pdsc file '{}' in CMSIS Pack {}: {}",
            pdsc_file.name(),
            path.display(),
            e
        )
    })?;

    drop(pdsc_file);

    extract_families(package, Kind::Archive(&mut archive), families, false)
}

pub async fn visit_arm_files(families: &mut Vec<ChipFamily>, filter: Option<String>) -> Result<()> {
    //TODO: The multi-threaded logging makes it very difficult to track which errors/warnings belong where - needs some rework.
    let packs = crate::fetch::get_vidx().await?;

    let mut stream =
        futures::stream::iter(packs.pdsc_index.iter().enumerate().filter_map(|(i, pack)| {
            let only_supported_familes = if let Some(ref filter) = filter {
                // If we are filtering for specific filter patterns, then skip all the ones we don't want.
                if !pack.name.contains(filter) {
                    log::debug!("Ignoring filtered {} ...", pack.name);
                    return None;
                }

                log::info!("Found matching chip family: {}", pack.name);

                // If we are filtering for specific filter patterns, then do not restrict these to the list of supported families.
                false
            } else {
                // If we are not filtering for specific filter patterns, then only include the supported families.
                true
            };
            if pack.deprecated.is_none() {
                // We only want to download the pack if it is not deprecated.
                log::info!("Working PACK {}/{} ...", i, packs.pdsc_index.len());
                Some(visit_arm_file(pack, only_supported_familes))
            } else {
                log::warn!("Ignoring deprecated {} ...", pack.name);
                None
            }
        }))
        .buffer_unordered(32);
    while let Some(result) = stream.next().await {
        families.extend(result);
    }

    Ok(())
}

pub(crate) async fn visit_arm_file(
    pack: &PdscRef,
    only_supported_familes: bool,
) -> Vec<ChipFamily> {
    let url = format!(
        "{url}/{vendor}.{name}.{version}.pack",
        url = pack.url.trim_end_matches('/'),
        vendor = pack.vendor,
        name = pack.name,
        version = pack.version
    );

    log::info!("Downloading {url}");

    let response = match reqwest::get(&url).await {
        Ok(response) => response,
        Err(error) => {
            log::error!("Failed to download pack '{url}': {error}");
            return vec![];
        }
    };
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            log::error!("Failed to get bytes from pack '{url}': {error}");
            return vec![];
        }
    };

    log::info!("Trying to open pack file: {}.", url);
    let zip = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(zip) {
        Ok(archive) => archive,
        Err(error) => {
            log::error!("Failed to open pack '{url}': {error}");
            return vec![];
        }
    };

    let mut pdsc_file = match find_pdsc_in_archive(&mut archive) {
        Ok(Some(file)) => file,
        Ok(None) => {
            log::error!("Failed to find .pdsc file in archive {url}");
            return vec![];
        }
        Err(error) => {
            log::error!("Error handling archive {url}: {error}");
            return vec![];
        }
    };

    let mut pdsc = String::new();
    if let Err(error) = pdsc_file.read_to_string(&mut pdsc) {
        log::error!("Failed to read .pdsc file '{url}': {error}");
        return vec![];
    };

    let package = match Package::from_string(&pdsc) {
        Ok(package) => package,
        Err(error) => {
            log::error!(
                "Failed to parse pdsc file '{}' in CMSIS Pack {url}: {error}",
                pdsc_file.name(),
            );
            return vec![];
        }
    };

    let pdsc_name = pdsc_file.name().to_owned();

    drop(pdsc_file);

    let mut families = vec![];

    match extract_families(
        package,
        Kind::Archive(&mut archive),
        &mut families,
        only_supported_familes,
    ) {
        Ok(_) => log::info!("Processed package {pdsc_name}"),
        Err(error) => log::error!("Something went wrong while handling pack {url}: {error}"),
    };

    families
}

/// Extracts the pdsc out of a ZIP archive.
pub(crate) fn find_pdsc_in_archive<T>(
    archive: &mut zip::ZipArchive<T>,
) -> Result<Option<zip::read::ZipFile>>
where
    T: std::io::Seek + std::io::Read,
{
    let mut index = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let outpath = file.enclosed_name().ok_or_else(|| {
            anyhow!(
                "Error handling the ZIP file content with path '{}': Path seems to be malformed",
                file.name()
            )
        })?;

        if has_extension(&outpath, "pdsc") {
            // We cannot return the file directly here,
            // because this leads to lifetime problems.

            index = Some(i);
            break;
        }
    }

    if let Some(index) = index {
        let file = archive.by_index(index)?;

        Ok(Some(file))
    } else {
        Ok(None)
    }
}

/// A flag to indicate what type of memory this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryType {
    /// A RAM memory.
    Ram,
    /// A Non Volatile memory.
    Nvm,
    /// Generic
    Generic,
}

/// A struct to combine essential information from [`cmsis_pack::pdsc::Device::memories`].
/// This is used to apply the necessary sorting and filtering in creating [`MemoryRegion`]s.
// The sequence of the fields is important for the sorting by derived natural order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceMemory {
    memory_type: MemoryType,
    p_name: Option<String>,
    memory_start: u64,
    memory_end: u64,
    name: String,
    access: MemoryAccess,
}

impl DeviceMemory {
    fn access(&self) -> Option<MemoryAccess> {
        fn is_default(access: &MemoryAccess) -> bool {
            access == &MemoryAccess::default()
        }

        if is_default(&self.access) {
            None
        } else {
            Some(self.access)
        }
    }
}

/// Extracts the memory regions in the package.
/// The new memory regions are sorted by memory type, then by boot memory, then by start address,
/// with correctly assigned cores/processor names.
pub(crate) fn get_mem_map(device: &Device, cores: &[probe_rs_target::Core]) -> Vec<MemoryRegion> {
    let mut device_memories: Vec<DeviceMemory> = device
        .memories
        .0
        .iter()
        .map(|(name, memory)| DeviceMemory {
            name: name.clone(),
            p_name: memory.p_name.clone(),
            memory_type: if memory.default && memory.access.read && memory.access.write {
                MemoryType::Ram
            } else if memory.default
                && memory.access.read
                && memory.access.execute
                && !memory.access.write
            {
                MemoryType::Nvm
            } else {
                MemoryType::Generic
            },
            memory_start: memory.start,
            memory_end: memory.start + memory.size,
            access: MemoryAccess {
                read: memory.access.read,
                write: memory.access.write,
                execute: memory.access.execute,
                boot: memory.startup,
            },
        })
        .collect();

    // Sort by memory type, then by processor name, then by boot memory, then by start address.
    device_memories.sort_by_key(|memory| {
        (
            memory.memory_type,
            memory.p_name.clone(),
            memory.access.boot,
            memory.memory_start,
        )
    });

    let all_cores: Vec<_> = cores.iter().map(|core| core.name.clone()).collect();

    let is_multi_core = cores.len() > 1;

    // Convert DeviceMemory's to MemoryRegion's, and assign cores to shared reqions.
    let mut mem_map = vec![];
    for region in device_memories {
        if is_multi_core && region.p_name.is_none() {
            log::warn!("Device {}, memory region {} has no processor name, but this is required for a multicore device. Assigning memory to all cores!", device.name, region.name);
        }

        let cores = region
            .p_name
            .as_ref()
            .map(|s| vec![s.to_ascii_lowercase()])
            .unwrap_or_else(|| all_cores.clone());

        match region.memory_type {
            MemoryType::Ram => {
                if let Some(MemoryRegion::Ram(existing_region)) = mem_map.iter_mut().find(|existing_region| {
                        matches!(existing_region, MemoryRegion::Ram(ram_region) if ram_region.name.as_deref() == Some(&region.name) && ram_region.access == region.access())
                    })
                {
                    existing_region.cores.extend_from_slice(&cores);
                } else {
                    mem_map.push(MemoryRegion::Ram(RamRegion {
                        access: region.access(),
                        name: Some(region.name),
                        range: region.memory_start..region.memory_end,
                        cores,
                    }));
                }
            },
            MemoryType::Nvm => {
                if let Some(MemoryRegion::Nvm(existing_region)) = mem_map.iter_mut().find(|existing_region| {
                        matches!(existing_region, MemoryRegion::Nvm(nvm_region) if nvm_region.name.as_deref() == Some(&region.name) && nvm_region.access == region.access())
                    })
                {
                    existing_region.cores.extend_from_slice(&cores);
                } else {
                    mem_map.push(MemoryRegion::Nvm(NvmRegion {
                        access: region.access(),
                        name: Some(region.name),
                        range: region.memory_start..region.memory_end,
                        cores,
                        is_alias: false,
                    }));
                }
            },
            MemoryType::Generic => {
                if let Some(MemoryRegion::Generic(existing_region)) = mem_map.iter_mut().find(|existing_region| {
                        matches!(existing_region, MemoryRegion::Generic(generic_region) if generic_region.name.as_deref() == Some(&region.name) && generic_region.access == region.access())
                    })
                {
                    existing_region.cores.extend_from_slice(&cores);
                } else {
                    mem_map.push(MemoryRegion::Generic(GenericRegion {
                        access: region.access(),
                        name: Some(region.name),
                        range: region.memory_start..region.memory_end,
                        cores,
                    }));
                }
            },
        };
    }

    mem_map
}

fn patch_memmap(mem_map: &mut [MemoryRegion]) {
    ensure_single_ram_region_is_executable(mem_map);
}

/// Ensure that at least one RAM region is executable.
fn ensure_single_ram_region_is_executable(mem_map: &mut [MemoryRegion]) {
    // If the device only has one Ram region, mark that region as executable. This is necessary
    // as we rely on RAM-loaded flashing algorithms and so at least some of the RAM must be
    // executable.
    let ram_regions = mem_map
        .iter()
        .filter_map(MemoryRegion::as_ram_region)
        .count();

    if ram_regions == 1 {
        if let Some(MemoryRegion::Ram(ram_region)) = mem_map
            .iter_mut()
            .find(|region| matches!(region, MemoryRegion::Ram(_)))
        {
            if let Some(ref mut access) = ram_region.access {
                access.execute = true;
            }
        }
    }
}
