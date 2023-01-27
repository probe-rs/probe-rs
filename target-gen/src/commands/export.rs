use std::path::Path;

use anyhow::Result;
use xshell::{cmd, Shell};

use super::elf::cmd_elf;

pub fn cmd_export(
    target_artifact: &Path,
    template_path: &Path,
    definition_export_path: &Path,
) -> Result<()> {
    std::fs::copy(template_path, definition_export_path)?;
    cmd_elf(
        target_artifact,
        true,
        Some(definition_export_path),
        true,
        Some(String::from("algorithm-test")),
    )?;

    if let Err(error) = generate_debug_info(target_artifact) {
        println!("Generating debug artifacts failed because:");
        println!("{error}");
    }

    Ok(())
}

fn generate_debug_info(target_artifact: &Path) -> Result<()> {
    let sh = Shell::new()?;
    std::fs::write(
        "target/disassembly.s",
        cmd!(sh, "rust-objdump --disassemble {target_artifact}")
            .output()?
            .stdout,
    )?;
    std::fs::write(
        "target/dump.txt",
        cmd!(sh, "rust-objdump -x {target_artifact}")
            .output()?
            .stdout,
    )?;
    std::fs::write(
        "target/nm.txt",
        cmd!(sh, "rust-nm {target_artifact} -n").output()?.stdout,
    )?;

    Ok(())
}
