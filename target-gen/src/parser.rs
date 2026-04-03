use crate::flash_device::FlashDevice;
use anyhow::{Context, Result, anyhow};
use probe_rs_target::{FlashAlgorithmRelocationRange, FlashProperties, MemoryRange, RawFlashAlgorithm, SectorDescription};

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
    log::debug!("Trying to read {size} bytes from {address:#010x}.");

    let start = address as u64;
    let end = (address + size) as u64;
    let range_to_read = start..end;

    // Iterate all segments.
    for ph in &elf.program_headers {
        let segment_address = ph.p_paddr;
        let segment_size = ph.p_memsz.min(ph.p_filesz);

        log::debug!("Segment address: {segment_address:#010x}");
        log::debug!("Segment size:    {segment_size} bytes");

        let segment = segment_address..segment_address + segment_size;
        // If the requested data is not fully inside of the current segment, skip the segment.
        if !segment.contains_range(&range_to_read) {
            log::debug!("Skipping segment.");
            continue;
        }

        let start = ph.p_offset as u32 + address - segment_address as u32;
        return Some(&buffer[start as usize..][..size as usize]);
    }

    None
}

fn extract_flash_device(elf: &goblin::elf::Elf, buffer: &[u8]) -> Result<FlashDevice> {
    // Extract the flash device info.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        if name == "FlashDevice" {
            // This struct contains information about the FLM file structure.
            let address = sym.st_value as u32;
            return FlashDevice::new(elf, buffer, address);
        }
    }

    // Failed to find flash device
    Err(anyhow!("Failed to find 'FlashDevice' symbol in ELF file."))
}

/// Extracts a position & memory independent flash algorithm blob from the provided ELF file.
pub fn extract_flash_algo(
    existing_algo: Option<RawFlashAlgorithm>,
    buffer: &[u8],
    file_name: &std::path::Path,
    default: bool,
    fixed_load_address: bool,
) -> Result<RawFlashAlgorithm> {
    let mut algo = existing_algo.unwrap_or_default();

    let elf = goblin::elf::Elf::parse(buffer)?;

    let flash_device = extract_flash_device(&elf, buffer).context(format!(
        "Failed to extract flash information from ELF file '{}'.",
        file_name.display()
    ))?;

    // Extract binary blob.
    let algorithm_binary = crate::algorithm_binary::AlgorithmBinary::new(&elf, buffer)?;
    algo.instructions = algorithm_binary.blob();
    algo.address_relocation_ranges = compact_relocation_ranges(&algorithm_binary.address_relocations);

    let code_section_offset = algorithm_binary.code_section.start;

    // Extract the function pointers,
    // and check if a RTT symbol is present.
    for sym in elf.syms.iter() {
        let name = &elf.strtab[sym.st_name];

        match name {
            "Init" => algo.pc_init = Some(sym.st_value - code_section_offset as u64),
            "UnInit" => algo.pc_uninit = Some(sym.st_value - code_section_offset as u64),
            "EraseChip" => algo.pc_erase_all = Some(sym.st_value - code_section_offset as u64),
            "EraseSector" => algo.pc_erase_sector = sym.st_value - code_section_offset as u64,
            "ProgramPage" => algo.pc_program_page = sym.st_value - code_section_offset as u64,
            "Verify" => algo.pc_verify = Some(sym.st_value - code_section_offset as u64),
            "BlankCheck" => algo.pc_blank_check = Some(sym.st_value - code_section_offset as u64),
            // probe-rs additions
            "ReadFlash" => algo.pc_read = Some(sym.st_value - code_section_offset as u64),
            "FlashSize" => algo.pc_flash_size = Some(sym.st_value - code_section_offset as u64),
            "_SEGGER_RTT" => {
                algo.rtt_location = Some(sym.st_value);
                log::debug!("Found RTT control block at address {:#010x}", sym.st_value);
            }
            "PAGE_BUFFER" => {
                algo.data_load_address = Some(sym.st_value);
                log::debug!("Found PAGE_BUFFER at address {:#010x}", sym.st_value);
            }

            _ => {}
        }
    }

    apply_algorithm_binary_metadata(&mut algo, &algorithm_binary, fixed_load_address)?;

    algo.description.clone_from(&flash_device.name);
    algo.name = file_name
        .file_stem()
        .and_then(|f| f.to_str())
        .unwrap()
        .to_lowercase();
    algo.default = default;
    algo.flash_properties = FlashProperties::from(flash_device);
    algo.big_endian = !elf.little_endian;

    Ok(algo)
}

fn apply_algorithm_binary_metadata(
    algo: &mut RawFlashAlgorithm,
    algorithm_binary: &crate::algorithm_binary::AlgorithmBinary,
    fixed_load_address: bool,
) -> Result<()> {
    let code_section_offset = algorithm_binary.code_section.start;
    let data_section_offset = algorithm_binary.data_section.start - code_section_offset;
    let static_base_offset = algorithm_binary.static_base - code_section_offset;

    algo.data_section_offset = data_section_offset.into();
    algo.static_base_offset =
        (static_base_offset != data_section_offset).then_some(static_base_offset.into());

    if fixed_load_address {
        log::debug!(
            "Flash algorithm will be loaded at fixed address {:#010x}",
            algorithm_binary.code_section.load_address
        );

        anyhow::ensure!(
            algorithm_binary.is_continuous_in_ram(),
            "If the flash algorithm is not position independent, all sections have to follow each other in RAM. \
            Please check your linkerscript."
        );

        algo.load_address = Some(algorithm_binary.code_section.load_address as u64);
        algo.link_time_base_address = None;
        algo.address_relocation_ranges.clear();
    } else {
        algo.link_time_base_address = Some(algorithm_binary.link_time_base_address.into());
    }

    Ok(())
}

fn compact_relocation_ranges(
    relocations: &[crate::algorithm_binary::Relocation],
) -> Vec<FlashAlgorithmRelocationRange> {
    if relocations.is_empty() {
        return Vec::new();
    }

    let mut offsets: Vec<_> = relocations.iter().map(|relocation| u64::from(relocation.offset)).collect();
    offsets.sort_unstable();
    offsets.dedup();

    let mut ranges = Vec::new();
    let mut start = offsets[0];
    let mut end = start + 4;

    for offset in offsets.into_iter().skip(1) {
        if offset == end {
            end += 4;
        } else {
            ranges.push(FlashAlgorithmRelocationRange {
                offset: start,
                size: end - start,
            });
            start = offset;
            end = start + 4;
        }
    }

    ranges.push(FlashAlgorithmRelocationRange {
        offset: start,
        size: end - start,
    });

    ranges
}

impl From<FlashDevice> for FlashProperties {
    fn from(device: FlashDevice) -> Self {
        let sectors = device
            .sectors
            .iter()
            .map(|si| SectorDescription {
                address: si.address.into(),
                size: si.size.into(),
            })
            .collect();

        FlashProperties {
            address_range: device.start_address as u64
                ..(device.start_address as u64 + device.device_size as u64),

            page_size: device.page_size,
            erased_byte_value: device.erased_default_value,

            program_page_timeout: device.program_page_timeout,
            erase_sector_timeout: device.erase_sector_timeout,

            sectors,
        }
    }
}

#[cfg(test)]
mod test {
    use probe_rs_target::{FlashAlgorithmRelocationRange, FlashProperties, SectorDescription};

    use crate::algorithm_binary::{AlgorithmBinary, Relocation, Section};

    use super::apply_algorithm_binary_metadata;

    fn algorithm_binary(
        code_start: u32,
        data_start: u32,
        static_base: u32,
        link_time_base_address: u32,
        load_address: u32,
        address_relocations: Vec<Relocation>,
    ) -> AlgorithmBinary {
        AlgorithmBinary {
            code_section: Section {
                start: code_start,
                length: 0x20,
                data: vec![0; 0x20],
                load_address,
            },
            data_section: Section {
                start: data_start,
                length: 0x10,
                data: vec![0; 0x10],
                load_address: load_address + (data_start - code_start),
            },
            static_base,
            link_time_base_address,
            address_relocations,
            runtime_sections: vec![],
            runtime_start: code_start,
            runtime_end: data_start + 0x10,
        }
    }

    fn raw_algorithm() -> probe_rs_target::RawFlashAlgorithm {
        probe_rs_target::RawFlashAlgorithm {
            name: "algo".into(),
            description: "algo".into(),
            default: true,
            instructions: vec![],
            load_address: None,
            data_load_address: None,
            pc_init: Some(0),
            pc_uninit: Some(0),
            pc_program_page: 0,
            pc_erase_sector: 0,
            pc_erase_all: None,
            pc_verify: None,
            pc_blank_check: None,
            pc_read: None,
            pc_flash_size: None,
            data_section_offset: 0,
            static_base_offset: None,
            link_time_base_address: None,
            address_relocation_ranges: vec![],
            rtt_location: None,
            rtt_poll_interval: 20,
            flash_properties: FlashProperties {
                address_range: 0x0800_0000..0x0800_1000,
                page_size: 0x100,
                sectors: vec![SectorDescription {
                    size: 0x1000,
                    address: 0,
                }],
                ..Default::default()
            },
            cores: vec![],
            stack_size: None,
            stack_overflow_check: None,
            transfer_encoding: None,
            big_endian: false,
        }
    }

    #[test]
    fn extract_flash_algo_preserves_data_section_offset_and_sets_static_base_offset() {
        let mut raw = raw_algorithm();
        let algorithm_binary = algorithm_binary(
            0x1000,
            0x1040,
            0x1060,
            0x1000,
            0x2000_0100,
            vec![Relocation { offset: 0x8 }],
        );

        apply_algorithm_binary_metadata(&mut raw, &algorithm_binary, false).unwrap();

        assert_eq!(raw.data_section_offset, 0x40);
        assert_eq!(raw.static_base_offset, Some(0x60));
        assert_eq!(raw.link_time_base_address, Some(0x1000));
    }

    #[test]
    fn extract_flash_algo_sets_fixed_load_without_pic_base() {
        let mut raw = raw_algorithm();
        let algorithm_binary = algorithm_binary(
            0x1000,
            0x1040,
            0x1040,
            0x1000,
            0x2000_0100,
            vec![Relocation { offset: 0x8 }],
        );
        raw.address_relocation_ranges = vec![FlashAlgorithmRelocationRange {
            offset: 0x8,
            size: 0x4,
        }];

        apply_algorithm_binary_metadata(&mut raw, &algorithm_binary, true).unwrap();

        assert_eq!(raw.load_address, Some(0x2000_0100));
        assert_eq!(raw.data_section_offset, 0x40);
        assert_eq!(raw.static_base_offset, None);
        assert_eq!(raw.link_time_base_address, None);
        assert!(raw.address_relocation_ranges.is_empty());
    }
}
