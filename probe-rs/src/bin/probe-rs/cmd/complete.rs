use std::io::{Cursor, Write};

use anyhow::{anyhow, Context};
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use probe_rs::probe::list::Lister;

use crate::{util::common_options::OperationError, Cli};

const BIN_NAME: &str = "probe-rs";

/// Install and complete autocomplete scripts
#[derive(clap::Parser)]
#[clap(verbatim_doc_comment)]
pub struct Cmd {
    #[clap(long)]
    shell: Option<Shell>,
    #[clap(subcommand)]
    kind: CompleteKind,
}

impl Cmd {
    pub fn run(&self, lister: &Lister) -> Result<(), anyhow::Error> {
        let shell = Shell::from_env()
            .or(self.shell)
            .ok_or_else(|| anyhow!("The current shell could not be determined. Please specify a shell with the --shell argument."))?;
        if !matches!(shell, Shell::Zsh | Shell::Bash) {
            anyhow::bail!("Only ZSH and Bash are supported for autocompletions at the moment");
        }

        match &self.kind {
            CompleteKind::Install => {
                let mut command = <Cli as CommandFactory>::command();
                let name = std::env::args_os().next().unwrap();
                let name = name.to_str().unwrap().split('/').last().unwrap();
                command = command.name("probe-rs");
                let mut script = Cursor::new(Vec::<u8>::new());
                generate(shell, &mut command, name, &mut script);
                let mut script = String::from_utf8_lossy(&script.into_inner()).to_string();
                inject_dynamic_completions(shell, name, &mut script)?;

                match shell {
                    Shell::Zsh => {
                        let Some(dir) = directories::UserDirs::new() else {
                            eprintln!("User home directory could not be located.");
                            eprintln!("Install script in ~/.zfunc/_{BIN_NAME}");
                            println!("{script}");
                            return Ok(());
                        };
                        let dir = dir.home_dir();
                        std::fs::write(dir.join(format!(".zfunc/_{BIN_NAME}")), &script)
                            .context("Writing the autocompletion script failed.")?;
                    }

                    Shell::Bash => todo!(),
                    _ => unreachable!(),
                }
            }
            CompleteKind::ProbeList { input } => {
                let mut script = Cursor::new(Vec::<u8>::new());
                list_probes(&mut script, lister, input)?;
                let output = String::from_utf8_lossy(&script.into_inner()).to_string();
                println!("{output}");
            }
            CompleteKind::ChipList { input } => {
                let mut script = Cursor::new(Vec::<u8>::new());
                list_chips(&mut script, input)?;
                let output = String::from_utf8_lossy(&script.into_inner()).to_string();
                println!("{output}");
            }
        };

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, clap::Subcommand)]
#[clap(verbatim_doc_comment)]
pub enum CompleteKind {
    /// Installs the autocomplete script for the correct shell.
    Install,
    /// Lists the probes that are currently plugged in in a way that the shell understands.
    ProbeList {
        /// The already entered user input that will be used to filter the list.
        #[clap()]
        input: String,
    },
    /// Lists the chips in a way that the shell understands.
    ChipList {
        /// The already entered user input that will be used to filter the list.
        #[clap()]
        input: String,
    },
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

/// Inject the dynamic completion portion.
fn inject_dynamic_completions(
    shell: Shell,
    name: &str,
    script: &mut String,
) -> Result<(), anyhow::Error> {
    match shell {
        Shell::Zsh => {
            inject_dynamic_zsh_script(script, name)?;
        }
        Shell::Bash => {
            inject_dynamic_bash_script(script)?;
        }
        _ => {}
    }
    Ok(())
}

/// Inject the dynamic completion portion of the bash script.
fn inject_dynamic_bash_script(script: &mut String) -> Result<(), anyhow::Error> {
    dynamic_complete_bash_attribute(script, "chip-list", r#"\-\-chips"#)?;
    dynamic_complete_bash_attribute(script, "probe-list", r#"\-\-probe"#)?;
    Ok(())
}

/// Inject the dynamic completion portion of a single selector for bash.
fn dynamic_complete_bash_attribute(
    script: &mut String,
    command: &str,
    arg: &str,
) -> Result<(), anyhow::Error> {
    let re = regex::Regex::new(&format!(
        r#"(?s)({arg}\)\n *COMPREPLY=\(\$\()compgen \-f( "\$\{{cur\}}"\)\))"#
    ))?;
    *script = re
        .replace_all(
            script,
            &format!(r#"${{1}}{BIN_NAME} complete {command} $2"#),
        )
        .into();
    Ok(())
}

/// Inject the dynamic completion portion of the ZSH script.
fn inject_dynamic_zsh_script(script: &mut String, name: &str) -> Result<(), anyhow::Error> {
    let re = regex::Regex::new(&format!(r#"(_{name} "\$@")"#))?;
    let inject = format!(
        "{}\n{}",
        dynamic_complete_zsh_attribute("chip_list", "chip-list"),
        dynamic_complete_zsh_attribute("probe_list", "probe-list")
    );
    *script = re.replace_all(script, format!("{inject}\n$1")).into();
    replace_zsh_complete_types(script, "PROBE_SELECTOR", "probe_list")?;
    replace_zsh_complete_types(script, "CHIP", "chip_list")?;
    Ok(())
}

/// Injects a ZSH function for listing all possible values for a selector value.
///
/// Required in conjunction with [`replace_zsh_complete_types`].
fn dynamic_complete_zsh_attribute(fn_name: &str, command: &str) -> String {
    format!(
        r#"(( $+functions[_{BIN_NAME}_{fn_name}] )) ||
        _{BIN_NAME}_{fn_name}() {{
            array_of_lines=("$${{(@f)$$({BIN_NAME} complete zsh {command} "" )}}")
            _values 'flags' $$array_of_lines
        }}"#
    )
}

/// Replaces the flag selectors with the functions injected with [`dynamic_complete_zsh_attribute`].
fn replace_zsh_complete_types(
    script: &mut String,
    selector: &str,
    fn_name: &str,
) -> Result<(), anyhow::Error> {
    let re = regex::Regex::new(&format!("({selector}: )"))?;
    *script = re
        .replace_all(script, format!("{selector}:_{BIN_NAME}_{fn_name}"))
        .into();
    Ok(())
}
