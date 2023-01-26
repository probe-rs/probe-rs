use std::path::PathBuf;

use anyhow::Result;
use xshell::{cmd, Shell};

use crate::cargo::generate_elf;

use super::elf::cmd_elf;

pub const DEFINITION_EXPORT_PATH: &str = "target/definition.yaml";

pub fn cmd_export() -> Result<()> {
    let target_artifact = generate_elf()?;

    let sh = Shell::new()?;

    cmd!(sh, "cp template.yaml {DEFINITION_EXPORT_PATH}").run()?;
    cmd_elf(
        PathBuf::from(&target_artifact),
        true,
        Some(PathBuf::from(DEFINITION_EXPORT_PATH)),
        true,
        Some(String::from("algorithm-test")),
    )?;

    generate_debug_info(&sh, &target_artifact)?;

    Ok(())
}

fn generate_debug_info(sh: &Shell, target_artifact: &str) -> Result<()> {
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
