use super::{GdbErrorExt, RuntimeTarget};

use anyhow::anyhow;

use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::TargetError;

use probe_rs::config::MemoryRegion;
use probe_rs::{CoreType, InstructionSet, Session};

fn copy_to_buf(data: &[u8], buf: &mut [u8]) -> usize {
    let len = data.len();
    let buf = &mut buf[..len];
    buf.copy_from_slice(data);
    len
}

fn copy_range_to_buf(data: &[u8], offset: u64, length: usize, buf: &mut [u8]) -> usize {
    let offset = match usize::try_from(offset) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let len = data.len();
    let data = &data[len.min(offset)..len.min(offset + length)];
    copy_to_buf(data, buf)
}

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

        let mut session = self.session.borrow_mut();
        let mut core = session.core(self.cores[0]).into_target_result()?;

        let xml = build_target_description(
            core.core_type(),
            core.instruction_set().into_target_result()?,
        );
        let xml_data = xml.as_bytes();

        Ok(copy_range_to_buf(xml_data, offset, length, buf))
    }
}

impl MemoryMap for RuntimeTarget<'_> {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let mut session = self.session.borrow_mut();
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

/// Build the GDB target description XML for a core type and ISA
fn build_target_description(core_type: CoreType, isa: InstructionSet) -> String {
    // GDB-architectures
    //
    // - armv6-m      -> Core-M0
    // - armv7-m      -> Core-M3
    // - armv7e-m      -> Core-M4, Core-M7
    // - armv8-m.base -> Core-M23
    // - armv8-m.main -> Core-M33
    // - riscv:rv32   -> RISCV

    let architecture = match core_type {
        CoreType::Armv6m => "armv6-m",
        CoreType::Armv7a => "armv7",
        CoreType::Armv7m => "armv7",
        CoreType::Armv7em => "armv7e-m",
        CoreType::Armv8a => match isa {
            InstructionSet::A64 => "aarch64",
            _ => "armv8-a",
        },
        CoreType::Armv8m => "armv8-m.main",
        CoreType::Riscv => "riscv:rv32",
    };

    // Only target.xml is supported
    let mut target_description = r#"<?xml version="1.0"?>
        <!DOCTYPE target SYSTEM "gdb-target.dtd">
        <target version="1.0">
        "#
    .to_owned();

    target_description.push_str(&format!("<architecture>{}</architecture>", architecture));

    target_description.push_str("</target>");

    target_description
}

#[cfg(test)]
mod test {
    use super::{build_target_description, CoreType, InstructionSet};

    #[test]
    fn test_target_description_microbit() {
        let description = build_target_description(CoreType::Armv6m, InstructionSet::Thumb2);

        insta::assert_snapshot!(description);
    }
}
