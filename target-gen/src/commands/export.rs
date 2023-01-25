use anyhow::Result;
use xshell::{cmd, Shell};

use crate::cargo::generate_elf;

pub const DEFINITION_EXPORT_PATH: &str = "target/definition.yaml";

pub fn cmd_export() -> Result<()> {
    let target_artifact = generate_elf()?;

    let sh = Shell::new()?;

    cmd!(sh, "cp template.yaml {DEFINITION_EXPORT_PATH}").run()?;
    cmd!(
        sh,
        "target-gen elf -n algorithm-test -u --fixed-load-address {target_artifact} {DEFINITION_EXPORT_PATH}"
    )
    .run()?;

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
