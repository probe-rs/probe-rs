use std::path::PathBuf;
use std::{fmt::Write, path::Path};

use anyhow::{anyhow, Context, Result};
use clap::CommandFactory;
use clap_complete::{
    generate,
    shells::{Bash, PowerShell, Zsh},
    Generator, Shell,
};
use probe_rs::probe::list::Lister;

use crate::Cli;

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
    /// Run the correct subcommand.
    pub fn run(&self, lister: &Lister) -> Result<()> {
        let shell = Shell::from_env()
            .or(self.shell)
            .ok_or_else(|| anyhow!("The current shell could not be determined. Please specify a shell with the --shell argument."))?;

        match &self.kind {
            CompleteKind::Install => {
                self.install(shell)?;
            }
            CompleteKind::ProbeList { input } => {
                self.probe_list(lister, input)?;
            }
            CompleteKind::ChipList { input } => {
                self.chips_list(input)?;
            }
        };

        Ok(())
    }

    /// Installs the autocompletion script for the currently active shell.
    ///
    /// If the shell cannot be determined or the auto-install is not implemented yet,
    /// the function prints the script with instructions for the user.
    pub fn install(&self, shell: Shell) -> Result<()> {
        let mut command = <Cli as CommandFactory>::command();
        let path: PathBuf = std::env::args_os().next().unwrap().into();
        let name = path.file_name().unwrap().to_str().unwrap();
        command = command.name("probe-rs");
        let mut script = Vec::<u8>::new();
        generate(shell, &mut command, name, &mut script);
        let mut script = String::from_utf8_lossy(&script).to_string();
        inject_dynamic_completions(shell, name, &mut script)?;

        let file_name = shell.file_name(BIN_NAME);

        match shell {
            Shell::Zsh => {
                Zsh.install(&file_name, &script)?;
            }
            Shell::Bash => {
                Zsh.install(&file_name, &script)?;
            }
            Shell::PowerShell => {
                PowerShell.install(&file_name, &script)?;
            }
            shell => {
                println!("{script}");
                eprintln!("{shell} does not have automatic install support yet.");
                eprintln!("Please install the script above in the appropriate location.");
            }
        }

        Ok(())
    }

    /// List all the found probes in a format the shell autocompletion understands.
    fn probe_list(&self, lister: &Lister, input: &str) -> Result<()> {
        println!("{}", list_probes(lister, input)?);
        Ok(())
    }

    /// List all the found chips in a format the shell autocompletion understands.
    fn chips_list(&self, input: &str) -> Result<()> {
        println!("{}", list_chips(input)?);
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
pub fn list_chips(starts_with: &str) -> Result<String> {
    let mut output = String::new();
    for family in probe_rs::config::families() {
        for variant in family.variants() {
            if variant.name.starts_with(starts_with) {
                writeln!(output, "{}", variant.name)?;
            }
        }
    }
    Ok(output)
}

/// Lists all the probes that are available for autocompletion to read.
/// This are all the probes that are currently connected.
///
/// Output will be one line per probe and print the PID:VID:SERIAL and the full name.
pub fn list_probes(lister: &Lister, starts_with: &str) -> Result<String> {
    let mut output = String::new();
    let probes = lister.list_all();
    for probe in probes {
        if probe.identifier.starts_with(starts_with) {
            writeln!(
                &mut output,
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
    Ok(output)
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
    dynamic_complete_bash_attribute(script, "chip-list", r#"\-\-chip"#)?;
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
        .replace_all(script, &format!(r#"${{1}}{BIN_NAME} complete {command}$2"#))
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

trait ShellExt {
    fn install(&self, file_name: &str, script: &str) -> Result<()>;
}

impl ShellExt for Zsh {
    fn install(&self, file_name: &str, script: &str) -> Result<()> {
        let Some(dir) = directories::UserDirs::new() else {
            println!("{script}");
            eprintln!("The user home directory could not be located.");
            eprintln!("Write the script to ~/.zfunc/{file_name}");
            eprintln!("Install the autocompletion by reloading the zsh");
            return Ok(());
        };

        let path = dir.home_dir().join(".zfunc/").join(file_name);
        write_script(&path, script)?;
        use std::io::Write;

        // Check if .zfunc is in FPATH
        if let Ok(fpath) = std::env::var("FPATH") {
            if !fpath.split(':').any(|p| p == path.to_str().unwrap()) {
                let zshrc_path = dir.home_dir().join(".zshrc");
                let export_cmd = r#"
# Add .zfunc to FPATH for autocompletion
export FPATH="$HOME/.zfunc:$FPATH"
"#;
                let result = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&zshrc_path)
                    .and_then(|mut file| writeln!(file, "{}", export_cmd))
                    .context("Failed to update .zshrc with FPATH");

                match result {
                    Ok(_) => eprintln!("Added .zfunc to FPATH in .zshrc. Please reload your zsh."),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Please add the following line to your .zshrc manually:");
                        eprintln!("{}", export_cmd);
                    }
                }
            }
        }

        Ok(())
    }
}

impl ShellExt for Bash {
    fn install(&self, file_name: &str, script: &str) -> Result<()> {
        let Some(dir) = directories::UserDirs::new() else {
            println!("{script}");
            eprintln!("The user home directory could not be located.");
            eprintln!("Write the script to ~/.bash_completion/{file_name}");
            eprintln!("Install the autocompletion by reloading the bash");
            return Ok(());
        };

        let path = dir.home_dir().join(".bash_completions/").join(file_name);
        write_script(&path, script)
    }
}

impl ShellExt for PowerShell {
    fn install(&self, file_name: &str, script: &str) -> Result<()> {
        let Some(dir) = directories::UserDirs::new() else {
            println!("{script}");
            eprintln!("The user home directory could not be located.");
            eprintln!("Write the script to ~\\Documents\\WindowsPowerShell\\{file_name}");
            eprintln!("Install the autocompletion with `Import-Module ~\\Documents\\WindowsPowerShell\\{file_name}`");
            return Ok(());
        };
        let path = dir
            .home_dir()
            .join("Documents")
            .join("WindowsPowerShell")
            .join(file_name);
        eprintln!(
            "Install the autocompletion with `Import-Module {}`",
            path.display()
        );
        write_script(&path, script)
    }
}

fn write_script(path: &Path, script: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent).context("Failed to create directory") {
                println!("{script}");
                eprintln!("Creating the parent directories failed: {}", e);
                eprintln!(
                    "Please create the parent directories and write the above script to {} manually",
                    path.display()
                );
                return Err(e);
            }
        }
    }

    let res = std::fs::write(path, script);
    if res.is_err() {
        println!("{script}");
        eprintln!("Writing the autocompletion script failed");
        eprintln!(
            "Please write the above script to {} manually",
            path.display()
        );
    }

    res.context("Writing the script failed")
}
