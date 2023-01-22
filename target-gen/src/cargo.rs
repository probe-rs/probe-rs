use anyhow::Result;
use cargo_metadata::Message;
use xshell::{cmd, Shell};

pub fn generate_elf() -> Result<String> {
    // We build the actual flash algorithm.
    // We relay all the output of the build process to the open shell.
    let sh = Shell::new()?;
    let mut cmd = cmd!(
        sh,
        "cargo build --release --message-format=json-diagnostic-rendered-ansi"
    );
    cmd.set_ignore_status(true);
    let output = cmd.output()?;
    print!("{}", String::from_utf8_lossy(&output.stderr));

    // Parse build information to extract the artifcat.
    let messages = Message::parse_stream(output.stdout.as_ref());

    // Find artifacts.
    let mut target_artifact = None;
    for message in messages {
        match message? {
            Message::CompilerArtifact(artifact) => {
                if let Some(executable) = artifact.executable {
                    if target_artifact.is_some() {
                        // We found multiple binary artifacts,
                        // so we don't know which one to use.
                        // This should never happen!
                        unreachable!()
                    } else {
                        target_artifact = Some(executable);
                    }
                }
            }
            Message::CompilerMessage(message) => {
                if let Some(rendered) = message.message.rendered {
                    print!("{}", rendered);
                }
            }
            // Ignore other messages.
            _ => (),
        }
    }
    let target_artifact = target_artifact.expect("a flash algorithm artifact");
    Ok(target_artifact.into_string())
}
