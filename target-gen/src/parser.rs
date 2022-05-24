use crate::flash_device::FlashDevice;
use probe_rs::config::{FlashProperties, RawFlashAlgorithm, SectorDescription};

use anyhow::{anyhow, Context, Result};

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

        log::debug!("Segment address: {:#010x}", segment_address);
        log::debug!("Segment size:    {} bytes", segment_size);

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

fn extract_flash_device(elf: &goblin::elf::Elf, buffer: &[u8]) -> Result<FlashDevice> {
    // Extract the flash device info.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        if let "FlashDevice" = name {
            // This struct contains information about the FLM file structure.
            let address = sym.st_value as u32;
            return FlashDevice::new(elf, buffer, address);
        }
    }

    // Failed to find flash device
    Err(anyhow!("Failed to find 'FlashDevice' symbol in ELF file."))
}

/// Extracts a position & memory independent flash algorithm blob from the proveided ELF file.
pub fn extract_flash_algo(
    mut file: impl std::io::Read,
    file_name: &std::path::Path,
    default: bool,
) -> Result<RawFlashAlgorithm> {
    let mut buffer = vec![];
    file.read_to_end(&mut buffer)?;

    let mut algo = RawFlashAlgorithm::default();

    let elf = goblin::elf::Elf::parse(buffer.as_slice())?;

    let flash_device = extract_flash_device(&elf, &buffer).context(format!(
        "Failed to extract flash information from ELF file '{}'.",
        file_name.display()
    ))?;

    // Extract binary blob.
    let algorithm_binary = crate::algorithm_binary::AlgorithmBinary::new(&elf, &buffer)?;
    algo.instructions = algorithm_binary.blob();

    let code_section_offset = algorithm_binary.code_section.start;

    // Extract the function pointers.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        match name {
            "Init" => algo.pc_init = Some(sym.st_value - code_section_offset as u64),
            "UnInit" => algo.pc_uninit = Some(sym.st_value - code_section_offset as u64),
            "EraseChip" => algo.pc_erase_all = Some(sym.st_value - code_section_offset as u64),
            "EraseSector" => algo.pc_erase_sector = sym.st_value - code_section_offset as u64,
            "ProgramPage" => algo.pc_program_page = sym.st_value - code_section_offset as u64,
            _ => {}
        }
    }

    algo.description = flash_device.name;

    algo.name = file_name
        .file_stem()
        .and_then(|f| f.to_str())
        .unwrap()
        .to_lowercase();
    algo.default = default;
    algo.data_section_offset = algorithm_binary.data_section.start as u64;

    let sectors = flash_device
        .sectors
        .iter()
        .map(|si| SectorDescription {
            address: si.address.into(),
            size: si.size.into(),
        })
        .collect();

    let properties = FlashProperties {
        address_range: flash_device.start_address as u64
            ..(flash_device.start_address as u64 + flash_device.device_size as u64),

        page_size: flash_device.page_size,
        erased_byte_value: flash_device.erased_default_value,

        program_page_timeout: flash_device.program_page_timeout,
        erase_sector_timeout: flash_device.erase_sector_timeout,

        sectors,
    };

    algo.flash_properties = properties;

    Ok(algo)
}
