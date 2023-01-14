use clap::Parser;
use xshell::{cmd, Shell};

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{}", e);
        std::process::exit(-1);
    }
}

#[derive(clap::Parser)]
#[clap(
    about = "Various housekeeping and CLI scripts",
    author = "Noah Hüsser <yatekii@yatekii.ch> / Dominik Böhi <dominik.boehi@gmail.ch>"
)]
enum Cli {
    /// Fetches all the PRs since the current release which need a changelog.
    FetchPrs,
    /// Starts the release process for the given version by creating a new MR.
    Release {
        /// The version to be released in semver format.
        version: String,
    },
    /// Generate the output for the GH release from the different CHANGELOG.md's.
    GenerateReleaseChangelog {
        /// Use this when generating the changelog output locally to not
        /// automatically translate the GH API control characters and keep
        /// it human readable.
        #[clap(long)]
        local: bool,
    },
}

fn try_main() -> Result<(), DynError> {
    match Cli::parse() {
        Cli::FetchPrs => fetch_prs(),
        Cli::Release { version } => create_release_pr(version),
        Cli::GenerateReleaseChangelog { local } => generate_release_changelog(local),
    }
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

fn create_release_pr(version: String) -> Result<(), DynError> {
    let sh = Shell::new()?;

    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!(
        sh,
        "gh workflow run 'Open a release PR' --ref master -f version={version}"
    )
    .run()?;

    Ok(())
}

fn generate_release_changelog(local: bool) -> Result<(), DynError> {
    let probe_rs_changelog = extract_changelog_for_newest_version(
        local,
        &std::fs::read_to_string("CHANGELOG.md").unwrap(),
    );
    let cargo_flash_changelog = extract_changelog_for_newest_version(
        local,
        &std::fs::read_to_string("cargo-flash/CHANGELOG.md").unwrap(),
    );
    let cargo_embed_changelog = extract_changelog_for_newest_version(
        local,
        &std::fs::read_to_string("cargo-embed/CHANGELOG.md").unwrap(),
    );
    let cli_changelog = extract_changelog_for_newest_version(
        local,
        &std::fs::read_to_string("cli/CHANGELOG.md").unwrap(),
    );

    println!("# probe-rs (library)");
    println!("{probe_rs_changelog}");
    println!("# cargo-flash (cargo extension)");
    println!("{cargo_flash_changelog}");
    println!("# cargo-embed (cargo extension)");
    println!("{cargo_embed_changelog}");
    println!("# probe-rs-cli (CLI)");
    println!("{cli_changelog}");

    Ok(())
}

fn extract_changelog_for_newest_version(local: bool, changelog: &str) -> String {
    let re = regex::Regex::new(
        r"## \[\d+.\d+.\d+\]\n\n(?:Released \d+-\d+-\d+)?\n?\n?((?:[\n]|.)*?)## \[\d+.\d+.\d+\]",
    )
    .unwrap();
    let captures = re.captures(changelog).unwrap();
    let release_text = captures[1].trim_start().trim_end();

    // The GH API expects those special characters to be replaced.
    if !local {
        release_text.replace('%', "%25").replace('\n', "%0A")
    } else {
        release_text.to_string()
    }
}
