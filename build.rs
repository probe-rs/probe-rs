use std::{env, error::Error, fs, path::PathBuf, process::Command, str};

fn main() -> Result<(), Box<dyn Error>> {
    let out = &PathBuf::from(env::var("OUT_DIR")?);
    // NOTE(unwrap_or) user may not have `git` installed or this may be a crates.io checkout; don't
    // error in either case; just report an empty string
    fs::write(out.join("git-info.txt"), git_info().unwrap_or_default())?;

    Ok(())
}

fn git_info() -> Result<String, Box<dyn Error>> {
    let hash = Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()?;
    let date = Command::new("git")
        .args(&["log", "-1", "--format=%cs"])
        .output()?;

    Ok(if hash.status.success() && date.status.success() {
        format!(
            " ({} {})",
            str::from_utf8(&hash.stdout)?.trim(),
            str::from_utf8(&date.stdout)?.trim()
        )
    } else {
        String::new()
    })
}
