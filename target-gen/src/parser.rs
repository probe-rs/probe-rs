use crate::error::Error;
use probe_rs::config::flash_algorithm::RawFlashAlgorithm;
use probe_rs::config::flash_properties::FlashProperties;
use probe_rs::config::memory::SectorDescription;

use crate::flash_device::FlashDevice;

/// Extract a chunk of data from an ELF binary.
///
/// This does only return the data chunk if it is fully contained in one section.
/// If it is across two sections, no chunk will be returned.
pub(crate) fn read_elf_bin_data<'a>(
    elf: &'a goblin::elf::Elf<'_>,
    buffer: &'a [u8],
    address: u32,
    size: u32,
) -> Option<&'a [u8]> {
    // Iterate all segments.
    for ph in &elf.program_headers {
        let segment_address = ph.p_paddr as u32;
        let segment_size = ph.p_memsz.min(ph.p_filesz) as u32;

        // If the requested data is above the current segment, skip the segment.
        if address > segment_address + segment_size {
            continue;
        }

        // If the requested data is below the current segment, skip the segment.
        if address + size <= segment_address {
            continue;
        }

        // If the requested data chunk is fully contained in the segment, extract and return the data segment.
        if address >= segment_address && address + size <= segment_address + segment_size {
            let start = ph.p_offset as u32 + address - segment_address;
            return Some(&buffer[start as usize..][..size as usize]);
        }
    }

    None
}

fn extract_flash_device(elf: &goblin::elf::Elf, buffer: &[u8]) -> Option<FlashDevice> {
    // Extract the flash device info.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        if let "FlashDevice" = name {
            // This struct contains information about the FLM file structure.
            let address = sym.st_value as u32;
            return Some(FlashDevice::new(&elf, buffer, address));
        }
    }

    None
}

/// Extracts a position & memory independent flash algorithm blob from the proveided ELF file.
pub fn extract_flash_algo(
    mut file: impl std::io::Read,
    file_name: &std::path::Path,
    default: bool,
) -> Result<RawFlashAlgorithm, Error> {
    let mut buffer = vec![];
    file.read_to_end(&mut buffer).unwrap();

    let mut algo = RawFlashAlgorithm::default();

    let elf =
        goblin::elf::Elf::parse(&buffer.as_slice()).map_err(|e| Error::IoError(e.to_string()))?;

    let flash_device = extract_flash_device(&elf, &buffer)
        .ok_or_else(|| Error::IoError("Failed to read flash device".to_owned()))?;

    // Extract binary blob.
    let algorithm_binary = crate::algorithm_binary::AlgorithmBinary::new(&elf, &buffer)?;
    algo.instructions = algorithm_binary.blob_as_u32();

    // Extract the function pointers.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        match name {
            "Init" => algo.pc_init = Some(sym.st_value as u32),
            "UnInit" => algo.pc_uninit = Some(sym.st_value as u32),
            "EraseChip" => algo.pc_erase_all = Some(sym.st_value as u32),
            "EraseSector" => algo.pc_erase_sector = sym.st_value as u32,
            "ProgramPage" => algo.pc_program_page = sym.st_value as u32,
            _ => {}
        }
    }

    algo.description = flash_device.name;
    algo.name = file_name.file_stem().unwrap().to_str().unwrap().to_owned();
    algo.default = default;
    algo.data_section_offset = algorithm_binary.data_section.start;

    let sectors = flash_device
        .sectors
        .iter()
        .map(|si| SectorDescription {
            address: si.address,
            size: si.size,
        })
        .collect();

    let properties = FlashProperties {
        range: flash_device.start_address..(flash_device.start_address + flash_device.device_size),

        page_size: flash_device.page_size,
        erased_byte_value: flash_device.erased_default_value,

        program_page_timeout: flash_device.program_page_timeout,
        erase_sector_timeout: flash_device.erase_sector_timeout,

        sectors,
    };

    algo.flash_properties = properties;

    Ok(algo)
}
