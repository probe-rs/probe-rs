use std::path::{Path, PathBuf};

use anyhow::Result;
use xshell::{cmd, Shell};

use super::elf::cmd_elf;

pub const DEFINITION_EXPORT_PATH: &str = "target/definition.yaml";

pub fn cmd_export(target_artifact: PathBuf) -> Result<()> {
    std::fs::copy("template.yaml", DEFINITION_EXPORT_PATH)?;
    cmd_elf(
        target_artifact.clone(),
        true,
        Some(PathBuf::from(DEFINITION_EXPORT_PATH)),
        true,
        Some(String::from("algorithm-test")),
    )?;

    if let Err(error) = generate_debug_info(target_artifact.as_path()) {
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
