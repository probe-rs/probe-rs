use super::utils::copy_range_to_buf;
use super::{GdbErrorExt, RuntimeTarget};

use anyhow::anyhow;

use data::build_target_description;

use gdbstub::target::TargetError;
use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;

use probe_rs::Error;
use probe_rs::config::MemoryRegion;
use probe_rs::{CoreType, Session};

pub(crate) use data::{GdbRegisterSource, TargetDescription};

mod data;

impl TargetDescriptionXmlOverride for RuntimeTarget<'_> {
    fn target_description_xml(
        &self,
        annex: &[u8],
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        if annex != b"target.xml" {
            return Err(TargetError::Fatal(anyhow!(
                "Unsupported annex: '{}'",
                String::from_utf8_lossy(annex)
            )));
        }

        let xml = self.target_desc.get_target_xml();
        let xml_data = xml.as_bytes();

        Ok(copy_range_to_buf(xml_data, offset, length, buf))
    }
}

impl RuntimeTarget<'_> {
    pub(crate) fn load_target_desc(&mut self) -> Result<(), Error> {
        let mut session = self.session.lock();
        let mut core = session.core(self.cores[0])?;

        self.target_desc =
            build_target_description(core.registers(), core.core_type(), core.instruction_set()?);

        Ok(())
    }
}

impl MemoryMap for RuntimeTarget<'_> {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let mut session = self.session.lock();
        let xml = gdb_memory_map(&mut session, self.cores[0]).into_target_result()?;
        let xml_data = xml.as_bytes();

        Ok(copy_range_to_buf(xml_data, offset, length, buf))
    }
}

/// Compute GDB memory map for a session and primary core
fn gdb_memory_map(session: &mut Session, primary_core_id: usize) -> Result<String, Error> {
    let (virtual_addressing, address_size) = {
        let core = session.core(primary_core_id)?;
        let address_size = core.program_counter().size_in_bits();

        (
            // Cortex-A cores use virtual addressing
            matches!(core.core_type(), CoreType::Armv7a | CoreType::Armv8a),
            address_size,
        )
    };

    let mut xml_map = r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
"#.to_owned();

    if virtual_addressing {
        // GDB will not attempt to read / write anything outside the address map.
        // However, with virtual addressing any address could be valid.  As a result
        // we mark the entire address space as RAM since that's the best assumption
        // we can make.
        let region_entry = format!(
            r#"<memory type="ram" start="0x0" length="{:#x}"/>\n"#,
            match address_size {
                32 => u32::MAX as u64,
                64 => u64::MAX,
                _ => 0x0,
            }
        );

        xml_map.push_str(&region_entry);
    } else {
        for region in &session.target().memory_map {
            let region_kind = match region {
                MemoryRegion::Ram(_) => "ram",
                MemoryRegion::Generic(_) => "rom",
                MemoryRegion::Nvm(_) => {
                    if session.target().flash_algorithms.is_empty() {
                        "rom"
                    } else {
                        continue;
                    }
                }
            };
            let range = region.address_range();
            let start = range.start;
            let length = range.end - range.start;
            let region_entry = format!(
                r#"<memory type="{region_kind}" start="{start:#x}" length="{length:#x}"/>\n"#,
            );

            xml_map.push_str(&region_entry);
        }

        for algo in session.target().flash_algorithms.iter() {
            let start = algo.flash_properties.address_range.start;
            // Use the region end address (if possible) because it seems more correct
            // than the data in FlashProperties
            let end = if let Some(region) = session
                .target()
                .memory_region_by_address(algo.flash_properties.address_range.start)
            {
                region.address_range().end
            } else {
                algo.flash_properties.address_range.end
            };
            let mut sectors = algo.flash_properties.sectors.clone();
            sectors.sort_by_key(|s| s.address);
            // Add dummy sector at the end to calculate the length of the real last sector
            sectors.push(probe_rs::config::SectorDescription {
                size: 0,
                address: end - start,
            });
            // Pair each sector with the next one because we need the next sector
            // start address, which is the current one end address, to calculate the
            // sector size.
            for (current, next) in sectors.iter().zip(sectors.iter().skip(1)) {
                let start = current.address + start;
                let length = next.address - current.address;
                let blocksize = current.size;
                let region_entry = format!(
                    r#"<memory type="flash" start="{start:#x}" length="{length:#x}"><property name="blocksize">{blocksize:#x}</property></memory>\n"#
                );
                xml_map.push_str(&region_entry);
            }
        }
    }

    xml_map.push_str(r#"</memory-map>"#);

    Ok(xml_map)
}

#[cfg(test)]
mod test;
