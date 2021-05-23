use std::env;
use xshell::cmd;

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{}", e);
        std::process::exit(-1);
    }
}

fn try_main() -> Result<(), DynError> {
    let task = env::args().nth(1);
    match task.as_ref().map(|it| it.as_str()) {
        Some("release") => release(&env::args().nth(2).ok_or("Please add a release version.")?)?,
        _ => print_help(),
    }
    Ok(())
}

fn print_help() {
    eprintln!(
        "Tasks:
release            Performs the following steps to trigger a new release:
    1. Bump all probe-rs dependency numbers.
    2. Create a commit.
    3. Create a PR with a label.
"
    )
}

fn release(version: &str) -> Result<(), DynError> {
    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!("git checkout master").run()?;
    cmd!("git pull").run()?;

    // Bump the crate versions.
    cmd!("cargo workspaces version -y --no-git-tag --no-git-commit --no-git-push custom {version}")
        .run()?;

    // Checkout a release branch
    cmd!("git checkout -b v{version}").run()?;

    // Create the release commit.
    cmd!("git commit -a -m 'Prepare for the v{version} release.'").run()?;

    // Create the PR with a proper label, which then gets picked up by the CI.
    cmd!("gh pr create --label 'release-ready'").run()?;

    Ok(())
}
