use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context};
use clap::Parser;
use xshell::{cmd, Shell};

use anyhow::Result;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("\nError:");
        eprintln!("{e}");
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
    AssembleChangelog {
        /// The version to be released
        version: String,
        /// Force overwrite changelog even if it has local changes
        #[arg(long, default_value = "false")]
        force: bool,
        /// Do not delete used fragments
        #[arg(long)]
        no_cleanup: bool,
        #[arg(long)]
        commit: bool,
    },
    CheckChangelog {
        /// The number of the PR to check. If set, checks the PR changes, otherwise the local fragments.
        #[arg(long)]
        pr: Option<u64>,

        /// Whether to post a comment on the PR if there are issues. Requires `--pr` to be set.
        #[arg(long, default_value = "false", requires = "pr")]
        comment_error: bool,
    },
}

fn try_main() -> anyhow::Result<()> {
    match Cli::parse() {
        Cli::FetchPrs => fetch_prs()?,
        Cli::Release { version } => create_release_pr(version)?,
        Cli::AssembleChangelog {
            version,
            force,
            no_cleanup,
            commit,
        } => assemble_changelog(version, force, no_cleanup, commit)?,
        Cli::CheckChangelog { pr, comment_error } => check_changelog(pr, comment_error)?,
    }

    Ok(())
}

fn fetch_prs() -> Result<()> {
    let sh = Shell::new()?;

    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!(
        sh,
        "gh pr list --label 'needs-changelog' --state 'closed' --web --limit 300"
    )
    .run()?;

    Ok(())
}

fn create_release_pr(version: String) -> Result<()> {
    let sh = Shell::new()?;

    // Make sure we are on the master branch and we have the latest state pulled from our source of truth, GH.
    cmd!(
        sh,
        "gh workflow run 'Open a release PR' --ref master -f version={version}"
    )
    .run()?;

    Ok(())
}

const CHANGELOG_CATEGORIES: &[&str] = &["Added", "Changed", "Fixed", "Removed"];
const FRAGMENTS_DIR: &str = "changelog/";
const CHANGELOG_FILE: &str = "CHANGELOG.md";

#[derive(Debug)]
struct FragmentList {
    /// List of fragments, grouped by category
    fragments: HashMap<String, Vec<Fragment>>,

    /// List of invalid fragments (not matching the expected pattern)
    invalid_fragments: Vec<PathBuf>,
}

#[derive(Debug)]
struct Fragment {
    /// The number of the PR that added the fragment
    pr_number: Option<String>,
    /// The author of the PR that added the fragment
    author: Option<String>,
    /// The path to the fragment file
    path: PathBuf,
}

impl FragmentList {
    pub fn new() -> Self {
        let mut fragments = HashMap::new();

        for category in CHANGELOG_CATEGORIES {
            fragments.insert(category.to_lowercase(), Vec::new());
        }

        FragmentList {
            fragments,
            invalid_fragments: Vec::new(),
        }
    }

    fn is_ok(&self) -> bool {
        self.invalid_fragments.is_empty()
    }

    fn display(&self) -> String {
        let mut message = String::new();
        if self.invalid_fragments.is_empty() {
            writeln!(
                &mut message,
                "Found {} valid fragments:",
                self.fragments.len()
            )
            .unwrap();

            for (group, fragments) in self.fragments.iter() {
                if fragments.is_empty() {
                    continue;
                }

                writeln!(&mut message, " {group}:").unwrap();

                for fragment in fragments {
                    writeln!(
                        &mut message,
                        "  - {} (#{}) by @{}",
                        fragment.path.display(),
                        fragment.pr_number.as_deref().unwrap_or("<unknown>"),
                        fragment.author.as_deref().unwrap_or("<unknown>")
                    )
                    .unwrap();
                }
            }
        } else {
            message.push_str(
                "The following changelog fragments \
                do not match the expected pattern:\n",
            );

            for invalid_fragment in self.invalid_fragments.iter() {
                writeln!(&mut message, " - {}", invalid_fragment.display()).unwrap();
            }
            message.push('\n');

            message.push_str(
                "Files should start with one of the categories followed \
                by a dash, and end with '.md'\n\
                For example: 'added-foo-bar.md'\n\
                \n",
            );

            message.push_str("Valid categories are:\n");
            for category in CHANGELOG_CATEGORIES {
                writeln!(&mut message, " - {}", category.to_lowercase()).unwrap();
            }
        }

        message
    }
}

fn check_local_changelog_fragments(list: &mut FragmentList, fragments_dir: &Path) -> Result<()> {
    let fragment_files = std::fs::read_dir(fragments_dir)
        .with_context(|| format!("Unable to read fragments from {}", fragments_dir.display()))?;

    for file in fragment_files {
        let file = file?;
        let path = file.path();

        if !path.is_file() {
            continue;
        }

        let filename = path
            .file_name()
            .expect("All files should have a name")
            .to_str()
            .with_context(|| format!("Filename {} is not valid UTF-8", path.display()))?;

        if filename == ".gitkeep" {
            continue;
        }

        let Some((category, _)) = filename.split_once('-') else {
            // Unable to split filename
            list.invalid_fragments.push(path);
            continue;
        };

        let Some(fragments) = list.fragments.get_mut(category) else {
            // Incorrect caregory
            list.invalid_fragments.push(path);
            continue;
        };

        let sh = Shell::new()?;
        let sha = cmd!(sh, "git blame -l -s {path}").read()?;
        let sha = sha.split(' ').next().unwrap();
        println!("fetching PR info for sha: {sha}");

        let response = cmd!(sh, "gh api -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' https://api.github.com/repos/probe-rs/probe-rs/commits/{sha}/pulls").read()?;

        let json = serde_json::from_str::<serde_json::Value>(&response).unwrap();

        fragments.push(Fragment {
            pr_number: json[0]["number"].as_i64().map(|n| n.to_string()),
            author: json[0]["user"]["login"].as_str().map(|s| s.to_string()),
            path,
        });
    }

    println!();

    Ok(())
}

/// This function is similar to the above, but only checks for new changelog fragments in the PR.
fn check_new_changelog_fragments(list: &mut FragmentList, info: &PrInfo) -> Result<()> {
    for file in info
        .files
        .iter()
        .filter(|f| f.path.starts_with(FRAGMENTS_DIR))
    {
        let path = file.path.clone();

        let filename = path
            .file_name()
            .expect("All files should have a name")
            .to_str()
            .with_context(|| format!("Filename {} is not valid UTF-8", path.display()))?;

        let Some((category, _)) = filename.split_once('-') else {
            // Unable to split filename
            list.invalid_fragments.push(path);
            continue;
        };

        let Some(fragments) = list.fragments.get_mut(category) else {
            // Incorrect caregory
            list.invalid_fragments.push(path);
            continue;
        };

        fragments.push(Fragment {
            pr_number: Some(info.number.to_string()),
            author: Some(info.author.login.clone()),
            path,
        });
    }

    println!();

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct PrInfo {
    number: u64,
    author: PrAuthor,
    labels: Vec<Label>,
    files: Vec<PrFile>,
}

#[derive(Debug, serde::Deserialize)]
struct PrAuthor {
    login: String,
}

#[derive(Debug, serde::Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct PrFile {
    path: PathBuf,
    additions: usize,
}

impl PrInfo {
    fn load(pr_number: u64) -> Result<Self> {
        let sh = Shell::new()?;

        let pr = pr_number.to_string();
        let info_json = cmd!(sh, "gh pr view {pr} --json labels,files,author,number").read()?;
        let pr_info = serde_json::from_str::<PrInfo>(&info_json)?;

        Ok(pr_info)
    }
}

fn check_changelog(pr_number: Option<u64>, comment_error: bool) -> Result<()> {
    let mut fragment_list = FragmentList::new();

    if let Some(pr_number) = pr_number {
        println!("Checking changelog fragments of PR {pr_number}");

        let info = PrInfo::load(pr_number)?;
        if info.labels.iter().any(|l| l.name == "skip-changelog") {
            println!("Skipping changelog check because of 'skip-changelog' label");
            return Ok(());
        }

        disallow_editing_main_changelog(&info)?;
        check_new_changelog_fragments(&mut fragment_list, &info)?;

        require_changelog_fragment(&info)?;
    } else {
        println!("No PR number, checking local fragments.");
        check_local_changelog_fragments(&mut fragment_list, Path::new(FRAGMENTS_DIR))?;
    }

    print_fragment_list(fragment_list, if comment_error { pr_number } else { None })?;

    println!("Everything looks good 👍");

    Ok(())
}

fn disallow_editing_main_changelog(info: &PrInfo) -> Result<()> {
    if info.labels.iter().any(|l| l.name == "release") {
        // The release PR is allowed to edit the main changelog.
        return Ok(());
    }

    if info
        .files
        .iter()
        .any(|f| f.path == Path::new(CHANGELOG_FILE))
    {
        let message = format!("Please do not edit {CHANGELOG_FILE} directly. Take a look at [CONTRIBUTING.md](https://github.com/probe-rs/probe-rs/blob/master/CONTRIBUTING.md) for information on changelog fragments instead.");

        write_comment(info.number, &message)?;
        anyhow::bail!("Please do not edit {CHANGELOG_FILE} directly");
    }

    Ok(())
}

fn write_comment(pr: u64, message: &str) -> Result<()> {
    let sh = Shell::new()?;
    let pr = pr.to_string();
    cmd!(sh, "gh pr comment {pr} -b {message}")
        .run()
        .context("Failed to comment on PR")?;

    Ok(())
}

fn require_changelog_fragment(info: &PrInfo) -> Result<()> {
    if !info
        .files
        .iter()
        .any(|f| f.path.starts_with(FRAGMENTS_DIR) && f.additions > 0)
    {
        anyhow::bail!(
            "No new changelog fragments detected, and 'skip-changelog' label not applied."
        );
    }

    Ok(())
}

/// `pr`: The PR number, if any, to comment on.
fn print_fragment_list(fragment_list: FragmentList, pr: Option<u64>) -> Result<()> {
    let message = fragment_list.display();
    println!("{message}");

    if !fragment_list.is_ok() {
        if let Some(pr) = pr {
            write_comment(pr, &message)?;
        }
    }

    Ok(())
}

fn is_changelog_unchanged() -> bool {
    let sh = Shell::new().unwrap();
    cmd!(sh, "git diff --exit-code {CHANGELOG_FILE}")
        .run()
        .is_ok()
}

fn assemble_changelog(
    version: String,
    force: bool,
    no_cleanup: bool,
    create_commit: bool,
) -> anyhow::Result<()> {
    if !force && !is_changelog_unchanged() {
        anyhow::bail!("Changelog has local changes, aborting.\nUse --force to override.");
    }

    let mut fragment_list = FragmentList::new();
    check_local_changelog_fragments(&mut fragment_list, Path::new(FRAGMENTS_DIR))?;

    ensure!(
        fragment_list.invalid_fragments.is_empty(),
        "Found invalid fragments: {:?}",
        fragment_list.invalid_fragments
    );

    let mut assembled = Vec::new();

    let mut writer = Cursor::new(&mut assembled);

    // Add an unreleased header, this will get picked up by `cargo-release` later.
    writeln!(writer, "## [Unreleased]")?;
    writeln!(writer)?;

    let mut fragments_found = false;

    for category in CHANGELOG_CATEGORIES {
        let fragment_list = fragment_list
            .fragments
            .get(&category.to_lowercase())
            .unwrap();

        if fragment_list.is_empty() {
            continue;
        }

        fragments_found = true;
        write_changelog_section(&mut writer, category, fragment_list)?;
    }

    ensure!(
        fragments_found,
        "No fragments found for changelog, aborting."
    );

    println!("Assembled changelog for version {}:", version);
    println!("{}", String::from_utf8(assembled.clone())?);

    let old_changelong_content = std::fs::read_to_string(CHANGELOG_FILE)?;

    let mut changelog_file = std::fs::File::create(CHANGELOG_FILE)?;

    let mut content_inserted = false;

    for line in old_changelong_content.lines() {
        if !content_inserted && line.starts_with("## ") {
            changelog_file.write_all(&assembled)?;
            content_inserted = true
        }

        writeln!(changelog_file, "{}", line)?;
    }

    println!("Changelog {} updated.", CHANGELOG_FILE);

    if !no_cleanup {
        println!("Cleaning up fragments...");

        for fragments in fragment_list.fragments.values() {
            for fragment in fragments {
                println!(" Removing {}", fragment.path.display());
                std::fs::remove_file(&fragment.path)?;
            }
        }
    }

    let shell = Shell::new()?;

    if create_commit && !no_cleanup {
        cmd!(shell, "git add {CHANGELOG_FILE}").run()?;
        cmd!(shell, "git rm {FRAGMENTS_DIR}/*.md").run()?;
        cmd!(
            shell,
            "git commit -m 'Update changelog for version '{version}"
        )
        .run()?;
    }

    Ok(())
}

fn write_changelog_section(
    mut writer: impl std::io::Write,
    heading: &str,
    fragments: &[Fragment],
) -> anyhow::Result<()> {
    writeln!(writer, "### {}", heading)?;
    writeln!(writer)?;

    for fragment in fragments {
        let text = std::fs::read_to_string(&fragment.path).with_context(|| {
            format!(
                "Failed to read changelog fragment {}",
                fragment.path.display()
            )
        })?;

        // Replace paragraph breaks which screw up list item spacing.
        let text = text.replace("\n\n", "<br>\n");

        let mut lines = text.lines();

        let Some(first_line) = lines.next() else {
            anyhow::bail!("Empty changelog fragment {}", fragment.path.display());
        };

        write!(writer, " - {}", first_line)?;

        // Write remaining lines
        for line in lines {
            writeln!(writer)?;
            write!(writer, "   {}", line)?;
        }

        if let Some(pr_number) = &fragment.pr_number {
            write!(writer, " (#{pr_number})")?;
        }

        if let Some(author) = &fragment.author {
            write!(writer, " by @{author}")?;
        }

        writeln!(writer)?;
    }

    writeln!(writer)?;

    Ok(())
}
