use std::io::{Cursor, Write};

use clap_complete::{generate, Shell};
use probe_rs::Probe;
use probe_rs_cli_util::{clap::CommandFactory, common_options::OperationError};

use crate::{Cli, CompleteKind};

/// Lists all the chips that are available for autocompletion to read.
///
/// Output will be one line per chip and print the full name probe-rs expects.
pub fn list_chips(mut f: impl Write, starts_with: String) -> Result<(), OperationError> {
    for family in probe_rs::config::families().map_err(OperationError::FailedToReadFamilies)? {
        for variant in family.variants() {
            if variant.name.starts_with(&starts_with) {
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
pub fn list_probes(mut f: impl Write, starts_with: String) -> Result<(), OperationError> {
    let probes = Probe::list_all();
    for probe in probes {
        if probe.identifier.starts_with(&starts_with) {
            writeln!(
                f,
                "{vid:04x}\\:{pid:04x}{sn}B[{id} \\[{typ:?}\\] B]",
                vid = probe.vendor_id,
                pid = probe.product_id,
                sn = probe
                    .serial_number
                    .clone()
                    .map_or("".to_owned(), |v| format!("\\:{}", v)),
                id = probe.identifier,
                typ = probe.probe_type
            )?;
        }
    }
    Ok(())
}

pub fn generate_completion(
    shell: Shell,
    kind: CompleteKind,
    input: String,
) -> Result<(), anyhow::Error> {
    if !matches!(shell, Shell::Zsh | Shell::Bash) {
        anyhow::bail!("Only ZSH and Bash are supported for autocompletions at the moment");
    }

    let output = match kind {
        CompleteKind::GenerateScript => {
            let mut command = <Cli as CommandFactory>::command();
            let name = std::env::args_os().next().unwrap();
            let name = name.to_str().unwrap().split('/').last().unwrap();
            command = command.name("probe-rs-cli");
            let mut script = Cursor::new(Vec::<u8>::new());
            generate(shell, &mut command, name, &mut script);
            let mut script = String::from_utf8_lossy(&script.into_inner()).to_string();
            inject_dynamic_completions(shell, name, &mut script)?;
            script
        }
        CompleteKind::ProbeList => {
            let mut script = Cursor::new(Vec::<u8>::new());
            list_probes(&mut script, input)?;
            String::from_utf8_lossy(&script.into_inner()).to_string()
        }
        CompleteKind::ChipList => {
            let mut script = Cursor::new(Vec::<u8>::new());
            list_chips(&mut script, input)?;
            String::from_utf8_lossy(&script.into_inner()).to_string()
        }
    };

    println!("{output}");

    Ok(())
}

fn inject_dynamic_completions(
    shell: Shell,
    name: &str,
    script: &mut String,
) -> Result<(), anyhow::Error> {
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

            let re = regex::Regex::new("(PROBE_SELECTOR: )")?;
            *script = re
                .replace_all(script, "PROBE_SELECTOR:_probe-rs-cli_probe_list")
                .into();

            let re = regex::Regex::new("(CHIP: )")?;
            *script = re
                .replace_all(script, "CHIP:_probe-rs-cli_chips_list")
                .into();
        }
        Shell::Bash => {
            let re = regex::Regex::new(
                r#"(?s)(\-\-chip\)\n *COMPREPLY=\(\$\()compgen \-f( "\$\{cur\}"\)\))"#,
            )?;
            *script = re
                .replace_all(script, r#"${1}probe-rs-cli complete chip-list $2"#)
                .into();
            let re = regex::Regex::new(
                r#"(?s)(\-\-probe\)\n *COMPREPLY=\(\$\()compgen \-f( "\$\{cur\}"\)\))"#,
            )?;
            *script = re
                .replace_all(script, r#"${1}probe-rs-cli complete probe-list $2"#)
                .into();
        }
        _ => {}
    }
    Ok(())
}
