use std::env;
use xshell::{cmd, Shell};

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{}", e);
        std::process::exit(-1);
    }
}

fn try_main() -> Result<(), DynError> {
    let task = env::args().nth(1);
    match task.as_deref() {
        Some("fetch-prs") => fetch_prs()?,
        Some("release") => create_release_pr(env::args().nth(2))?,
        _ => print_help(),
    }
    Ok(())
}

fn print_help() {
    eprintln!(
        "Tasks:
fetch-prs
    Help: Fetches all the PRs since the current release which need a changelog.
release
    Help: Starts the release process for the given version by creating a new MR.
"
    )
}

fn fetch_prs() -> Result<(), DynError> {
    let sh = Shell::new()?;

    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!(
        sh,
        "gh pr list --label 'needs-changelog' --state 'closed' --web --limit 300"
    )
    .run()?;

    Ok(())
}

fn create_release_pr(version: Option<String>) -> Result<(), DynError> {
    let sh = Shell::new()?;
    let version = version.expect("Version argument must be given.");

    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!(
        sh,
        "gh workflow run 'Open a release PR' --ref master -f version={version}"
    )
    .run()?;

    Ok(())
}
