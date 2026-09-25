pub(crate) mod data;
pub(crate) use data::{GdbRegisterSource, TargetDescription, build_target_description};

#[cfg(test)]
mod test;

use super::RuntimeTarget;
use super::utils::copy_range_to_buf;
use probe_rs::CoreType;
use probe_rs_rpc::chip::MemoryRegion;
use probe_rs_rpc::info::WireFlashSector;

use anyhow::anyhow;
use gdbstub::target::TargetError;
use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;

impl TargetDescriptionXmlOverride for RuntimeTarget {
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
        Ok(copy_range_to_buf(xml.as_bytes(), offset, length, buf))
    }
}

impl RuntimeTarget {
    pub(crate) fn load_target_desc(&mut self) -> Result<(), anyhow::Error> {
        let primary = self
            .cores
            .first()
            .ok_or_else(|| anyhow!("GDB stub has no cores"))?;

        self.target_desc = build_target_description(
            primary.registers,
            primary.core_type,
            primary.instruction_set,
        );
        Ok(())
    }

    pub(crate) fn build_memory_map_xml(&self) -> Result<String, anyhow::Error> {
        let primary = self
            .cores
            .first()
            .ok_or_else(|| anyhow!("GDB stub has no cores"))?;

        let address_size = primary
            .registers
            .pc()
            .map(|reg| reg.size_in_bits())
            .unwrap_or(32);

        Ok(gdb_memory_map_from_wire(
            &self.memory_map,
            &self.flash_sectors,
            primary.core_type,
            address_size,
        ))
    }
}

impl MemoryMap for RuntimeTarget {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let xml = self
            .memory_map_xml
            .as_deref()
            .ok_or_else(|| TargetError::Fatal(anyhow!("Memory map is not ready")))?;
        Ok(copy_range_to_buf(xml.as_bytes(), offset, length, buf))
    }
}

fn full_ram_memory_map(address_size: usize) -> String {
    let length = match address_size {
        64 => u64::MAX,
        _ => u32::MAX as u64,
    };

    format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
<memory type="ram" start="0x0" length="{length:#x}"/>
</memory-map>"#
    )
}

fn gdb_memory_map_from_wire(
    memory_map: &[MemoryRegion],
    flash_sectors: &[WireFlashSector],
    primary_core_type: CoreType,
    address_size: usize,
) -> String {
    // Cortex-A cores use virtual addressing; any address may be valid.
    if matches!(primary_core_type, CoreType::Armv7a | CoreType::Armv8a) {
        return full_ram_memory_map(address_size);
    }

    if memory_map.is_empty() && flash_sectors.is_empty() {
        return full_ram_memory_map(address_size);
    }

    let mut xml_map = r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
"#
    .to_owned();

    let has_flash = !flash_sectors.is_empty();
    for region in memory_map {
        let region_kind = match region {
            MemoryRegion::Ram(_) => "ram",
            MemoryRegion::Generic(_) => "rom",
            MemoryRegion::Nvm(_) => {
                if has_flash {
                    continue;
                } else {
                    "rom"
                }
            }
        };
        let range = region.address_range();
        let start = range.start;
        let length = range.end - range.start;
        xml_map.push_str(&format!(
            r#"<memory type="{region_kind}" start="{start:#x}" length="{length:#x}"/>\n"#
        ));
    }

    for sector in flash_sectors {
        xml_map.push_str(&format!(
            r#"<memory type="flash" start="{start:#x}" length="{length:#x}"><property name="blocksize">{blocksize:#x}</property></memory>\n"#,
            start = sector.start,
            length = sector.length,
            blocksize = sector.blocksize,
        ));
    }

    xml_map.push_str(r#"</memory-map>"#);
    xml_map
}
