use std::path::{Path, PathBuf};

use anyhow::Result;
use xshell::{cmd, Shell};

use super::elf::cmd_elf;

pub const DEFINITION_EXPORT_PATH: &str = "target/definition.yaml";

pub fn cmd_export(target_artifact: PathBuf) -> Result<()> {
    let sh = Shell::new()?;

    cmd!(sh, "cp template.yaml {DEFINITION_EXPORT_PATH}").run()?;
    cmd_elf(
        target_artifact.clone(),
        true,
        Some(PathBuf::from(DEFINITION_EXPORT_PATH)),
        true,
        Some(String::from("algorithm-test")),
    )?;

    generate_debug_info(&sh, target_artifact.as_path())?;

    Ok(())
}

fn generate_debug_info(sh: &Shell, target_artifact: &Path) -> Result<()> {
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
