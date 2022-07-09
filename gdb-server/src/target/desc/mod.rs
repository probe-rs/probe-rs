use super::{GdbErrorExt, RuntimeTarget};
use crate::target::utils::copy_range_to_buf;

mod data;

use anyhow::anyhow;

use data::build_target_description;

use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::TargetError;

use probe_rs::config::MemoryRegion;
use probe_rs::{CoreType, Session};

pub(crate) use data::{GdbRegisterSource, TargetDescription};

impl TargetDescriptionXmlOverride for RuntimeTarget<'_> {
    fn target_description_xml(
        &self,
        annex: &[u8],
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let annex = String::from_utf8_lossy(annex);
        if annex != "target.xml" {
            return Err(TargetError::Fatal(
                anyhow!("Unsupported annex: '{}'", annex).into(),
            ));
        }

        let xml = self.target_desc.get_target_xml();
        let xml_data = xml.as_bytes();

        Ok(copy_range_to_buf(xml_data, offset, length, buf))
    }
}

impl RuntimeTarget<'_> {
    pub(crate) fn load_target_desc(&mut self) -> Result<(), probe_rs::Error> {
        let mut session = self.session.lock().unwrap();
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
        let mut session = self.session.lock().unwrap();
        let xml = gdb_memory_map(&mut session, self.cores[0]).into_target_result()?;
        let xml_data = xml.as_bytes();

        Ok(copy_range_to_buf(xml_data, offset, length, buf))
    }
}

/// Compute GDB memory map for a session and primary core
fn gdb_memory_map(
    session: &mut Session,
    primary_core_id: usize,
) -> Result<String, probe_rs::Error> {
    let (virtual_addressing, address_size) = {
        let core = session.core(primary_core_id)?;
        let address_size = core.registers().program_counter().size_in_bits();

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
                32 => 0xFFFF_FFFFu64,
                64 => 0xFFFF_FFFF_FFFF_FFFF,
                _ => 0x0,
            }
        );

        xml_map.push_str(&region_entry);
    } else {
        for region in &session.target().memory_map {
            let region_entry = match region {
                MemoryRegion::Ram(ram) => format!(
                    r#"<memory type="ram" start="{:#x}" length="{:#x}"/>\n"#,
                    ram.range.start,
                    ram.range.end - ram.range.start
                ),
                MemoryRegion::Generic(region) => format!(
                    r#"<memory type="rom" start="{:#x}" length="{:#x}"/>\n"#,
                    region.range.start,
                    region.range.end - region.range.start
                ),
                MemoryRegion::Nvm(region) => {
                    // TODO: Use flash with block size
                    format!(
                        r#"<memory type="rom" start="{:#x}" length="{:#x}"/>\n"#,
                        region.range.start,
                        region.range.end - region.range.start
                    )
                }
            };

            xml_map.push_str(&region_entry);
        }
    }

    xml_map.push_str(r#"</memory-map>"#);

    Ok(xml_map)
}

#[cfg(test)]
mod test;
