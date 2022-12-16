use std::io::{Cursor, Write};

use clap::CommandFactory;
use clap_complete::{generate, Shell};
use probe_rs::probe::list::Lister;

use crate::{util::common_options::OperationError, Cli, CompleteKind};

#[derive(clap::Parser)]
#[clap(name = "complete")]
pub struct Cmd {
    #[clap()]
    shell: Shell,
    #[clap()]
    kind: CompleteKind,
    #[clap()]
    input: String,
}

impl Cmd {
    pub fn run(&self, lister: &Lister) -> Result<(), anyhow::Error> {
        if self.shell != Shell::Zsh {
            anyhow::bail!("Only ZSH is supported for autocompletions at the moment");
        }

        let output = match self.kind {
            CompleteKind::GenerateScript => {
                let mut command = <Cli as CommandFactory>::command();
                let name = std::env::args_os().next().unwrap();
                let name = name.to_str().unwrap().split('/').last().unwrap();
                let mut script = Cursor::new(Vec::<u8>::new());
                generate(self.shell, &mut command, name, &mut script);
                let mut script = String::from_utf8_lossy(&script.into_inner()).to_string();
                inject_dynamic_completions(self.shell, name, &mut script)?;
                script
            }
            CompleteKind::ProbeList => {
                let mut script = Cursor::new(Vec::<u8>::new());
                list_probes(&mut script, lister, &self.input)?;
                String::from_utf8_lossy(&script.into_inner()).to_string()
            }
            CompleteKind::ChipList => {
                let mut script = Cursor::new(Vec::<u8>::new());
                list_chips(&mut script, &self.input)?;
                String::from_utf8_lossy(&script.into_inner()).to_string()
            }
        };

        println!("{output}");

        Ok(())
    }
}

/// Lists all the chips that are available for autocompletion to read.
///
/// Output will be one line per chip and print the full name probe-rs expects.
pub fn list_chips(mut f: impl Write, starts_with: &str) -> Result<(), OperationError> {
    for family in probe_rs::config::families() {
        for variant in family.variants() {
            if variant.name.starts_with(starts_with) {
                writeln!(f, "{}", variant.name)?;
            }
        }
    }
    Ok(())
}

/// Lists all the probes that are available for autocompletion to read.
/// This are all the probes that are currently connected.
///
/// Output will be one line per probe and print the PID:VID:SERIAL and the full name.
pub fn list_probes(
    mut f: impl Write,
    lister: &Lister,
    starts_with: &str,
) -> Result<(), OperationError> {
    let probes = lister.list_all();
    for probe in probes {
        if probe.identifier.starts_with(starts_with) {
            writeln!(
                f,
                "{vid:04x}\\:{pid:04x}{sn}B[{id}B]",
                vid = probe.vendor_id,
                pid = probe.product_id,
                sn = probe
                    .serial_number
                    .clone()
                    .map_or("".to_owned(), |v| format!("\\:{}", v)),
                id = probe.identifier,
            )?;
        }
    }
    Ok(())
}

fn inject_dynamic_completions(
    shell: Shell,
    name: &str,
    script: &mut String,
) -> Result<(), anyhow::Error> {
    #[allow(clippy::single_match)]
    match shell {
        Shell::Zsh => {
            let re = regex::Regex::new(&format!(r#"(_{name} "\$@")"#))?;
            let inject = r#"(( $+functions[_probe-rs-cli_chips_list] )) ||
_probe-rs-cli_chips_list() {
    array_of_lines=("$${(@f)$$(probe-rs-cli complete zsh chip-list "" )}")
    _values 'flags' $$array_of_lines
}
(( $+functions[_probe-rs-cli_probe_list] )) ||
_probe-rs-cli_probe_list() {
    array_of_lines=("$${(@f)$$(probe-rs-cli complete zsh probe-list "" )}")
    if [ $${#array_of_lines[@]} -eq 0 ]; then
        _values 'flags' $$array_of_lines
    fi
}
            "#;
            *script = re.replace_all(script, format!("{inject}\n$1")).into();

            let re = regex::Regex::new(&format!(r#"(PROBE_SELECTOR: )"#))?;
            *script = re
                .replace_all(script, "PROBE_SELECTOR:_probe-rs-cli_probe_list")
                .into();

            let re = regex::Regex::new(&format!(r#"(CHIP: )"#))?;
            *script = re
                .replace_all(script, "CHIP:_probe-rs-cli_chips_list")
                .into();
        }
        _ => {}
    }
    Ok(())
}
