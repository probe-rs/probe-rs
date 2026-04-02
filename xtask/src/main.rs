use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use clap::Parser;
use regex::Regex;
use serde_json::Value;
use xshell::{Shell, cmd};

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
        /// The base version to be released in a.b.c format.
        version: String,
        /// Optional suffix appended as a.b.c-sifli.<suffix>.
        #[arg(long)]
        prerelease: Option<String>,
    },
    /// Creates a local sync branch from OpenSiFli/master and merges an upstream ref with --no-ff.
    SyncUpstream {
        /// The upstream tag or ref to merge, e.g. v0.30.0 or probe-rs/master.
        upstream_ref: String,
        /// Upstream remote to fetch before creating the sync branch.
        #[arg(long, default_value = "probe-rs")]
        upstream_remote: String,
        /// Base ref for the OpenSiFli release branch.
        #[arg(long, default_value = "OpenSiFli/master")]
        base_ref: String,
        /// Local branch name to create. Defaults to sync/<upstream-ref>.
        #[arg(long)]
        branch: Option<String>,
        /// Print the git commands without changing the repository.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
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
        Cli::Release {
            version,
            prerelease,
        } => create_release_pr(version, prerelease)?,
        Cli::SyncUpstream {
            upstream_ref,
            upstream_remote,
            base_ref,
            branch,
            dry_run,
        } => sync_upstream(upstream_ref, upstream_remote, base_ref, branch, dry_run)?,
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
        "gh pr list --label 'changelog:need' --state 'closed' --web --limit 300"
    )
    .run()?;

    Ok(())
}

fn create_release_pr(version: String, prerelease: Option<String>) -> Result<()> {
    let sh = Shell::new()?;

    // Make sure we are on the right branch and we have the latest state pulled from our source of truth, GH.

    let branch = cmd!(sh, "git branch --show-current")
        .read()?
        .trim()
        .to_string();

    if branch != "master" && !branch.starts_with("release/") {
        bail!(
            "Invalid current branch '{branch}'. Make sure you're either on `master` or a `release/*` branch."
        )
    }

    let mut command = format!(
        "gh workflow run 'Open a release PR' --ref {branch} -f version={version}"
    );

    if let Some(prerelease) = prerelease {
        write!(&mut command, " -f prerelease={prerelease}")?;
    }

    cmd!(sh, "{command}").run()?;

    Ok(())
}

fn sync_upstream(
    upstream_ref: String,
    upstream_remote: String,
    base_ref: String,
    branch: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let sh = Shell::new()?;
    let branch = branch.unwrap_or_else(|| default_sync_branch_name(&upstream_ref));

    ensure!(
        !branch.trim().is_empty(),
        "Derived sync branch name is empty. Pass --branch explicitly."
    );

    if dry_run {
        let message = format!("sync: merge {upstream_ref} from {upstream_remote}");

        println!("Dry run, no changes made.");
        println!("  git fetch OpenSiFli");
        println!("  git fetch {upstream_remote} --tags");
        println!("  git switch -c {branch} {base_ref}");
        println!("  git merge --no-ff --log -m \"{message}\" {upstream_ref}");
        return Ok(());
    }

    ensure_clean_worktree(&sh)?;

    cmd!(sh, "git fetch OpenSiFli").run()?;
    cmd!(sh, "git fetch {upstream_remote} --tags").run()?;

    ensure_ref_exists(&sh, &base_ref, "base ref")?;
    ensure_ref_exists(&sh, &upstream_ref, "upstream ref")?;
    ensure_valid_branch_name(&sh, &branch)?;
    ensure_local_branch_absent(&sh, &branch)?;

    let message = format!("sync: merge {upstream_ref} from {upstream_remote}");

    cmd!(sh, "git switch -c {branch} {base_ref}").run()?;
    cmd!(sh, "git merge --no-ff --log -m {message} {upstream_ref}").run()?;

    println!();
    println!("Prepared sync branch '{branch}'.");
    println!("Next steps:");
    println!("  1. Resolve conflicts and refresh any OpenSiFli-only patches.");
    println!("  2. Run CI and SiFli smoke tests on '{branch}'.");
    println!("  3. Push '{branch}' and open a PR into OpenSiFli/master once it is ready.");

    Ok(())
}

fn ensure_clean_worktree(sh: &Shell) -> Result<()> {
    let status = cmd!(sh, "git status --short").read()?;

    ensure!(
        status.trim().is_empty(),
        "Working tree has local changes. Commit or stash them before starting an upstream sync."
    );

    Ok(())
}

fn ensure_ref_exists(sh: &Shell, git_ref: &str, label: &str) -> Result<()> {
    let exists = cmd!(sh, "git rev-parse --verify --quiet {git_ref}")
        .quiet()
        .ignore_status()
        .output()?
        .status
        .success();

    ensure!(exists, "Unknown {label} '{git_ref}'.");

    Ok(())
}

fn ensure_valid_branch_name(sh: &Shell, branch: &str) -> Result<()> {
    let valid = cmd!(sh, "git check-ref-format --branch {branch}")
        .quiet()
        .ignore_status()
        .output()?
        .status
        .success();

    ensure!(
        valid,
        "Invalid branch name '{branch}'. Pass --branch with a valid local branch name."
    );

    Ok(())
}

fn ensure_local_branch_absent(sh: &Shell, branch: &str) -> Result<()> {
    let full_ref = format!("refs/heads/{branch}");
    let exists = cmd!(sh, "git show-ref --verify --quiet {full_ref}")
        .quiet()
        .ignore_status()
        .output()?
        .status
        .success();

    ensure!(
        !exists,
        "Local branch '{branch}' already exists. Delete it first or pass --branch with a new name."
    );

    Ok(())
}

fn default_sync_branch_name(upstream_ref: &str) -> String {
    let upstream_ref = upstream_ref
        .trim()
        .trim_start_matches("refs/tags/")
        .trim_start_matches("refs/heads/");
    let branch = sanitize_branch_component(upstream_ref);

    format!(
        "sync/{}",
        if branch.is_empty() {
            "upstream"
        } else {
            &branch
        }
    )
}

fn sanitize_branch_component(value: &str) -> String {
    let mut branch = String::with_capacity(value.len());
    let mut last_was_dash = false;

    for ch in value.chars() {
        let mapped = match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => Some(ch),
            '/' | ':' | ' ' => Some('-'),
            _ => Some('-'),
        };

        if let Some(ch) = mapped {
            if ch == '-' {
                if !last_was_dash {
                    branch.push(ch);
                }
                last_was_dash = true;
            } else {
                branch.push(ch);
                last_was_dash = false;
            }
        }
    }

    branch.trim_matches('-').to_string()
}

const CHANGELOG_CATEGORIES: &[&str] = &["Added", "Changed", "Fixed", "Removed"];
const FRAGMENTS_DIR: &str = "changelog/";
const CHANGELOG_FILE: &str = "CHANGELOG.md";
const SKIP_CHANGELOG_LABEL: &str = "changelog:skip";

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
        if self.is_ok() {
            self.ok_message()
        } else {
            self.error_message()
        }
    }

    fn ok_message(&self) -> String {
        let mut message = String::new();
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

        message
    }

    fn error_message(&self) -> String {
        let mut message = String::new();
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
        message
    }
}

fn check_local_changelog_fragments(list: &mut FragmentList, fragments_dir: &Path) -> Result<()> {
    let fragment_files = std::fs::read_dir(fragments_dir)
        .with_context(|| format!("Unable to read fragments from {}", fragments_dir.display()))?;

    let pr_number_regex = Regex::new(" \\(#(\\d+)\\)\n").unwrap();

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

        let commit_message = cmd!(
            sh,
            "git rev-list --max-count=1 --no-commit-header --format=%B {sha}"
        )
        .read()?;

        let mut pull = if let Some(m) = pr_number_regex.captures(&commit_message) {
            let pull = m.get(1).unwrap().as_str();
            let response = cmd!(sh, "gh api -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' https://api.github.com/repos/probe-rs/probe-rs/pulls/{pull}").read()?;
            serde_json::from_str::<serde_json::Value>(&response).unwrap()
        } else {
            let response = cmd!(sh, "gh api -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' https://api.github.com/repos/probe-rs/probe-rs/commits/{sha}/pulls").read()?;
            let json = serde_json::from_str::<serde_json::Value>(&response).unwrap();

            json.get(0).cloned().unwrap_or(Value::Null)
        };

        if pull["user"]["login"].as_str() == Some("probe-rs-bot") {
            pull = Value::Null
        }

        fragments.push(Fragment {
            pr_number: pull["number"].as_i64().map(|n| n.to_string()),
            author: pull["user"]["login"].as_str().map(|s| s.to_string()),
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
        if info.labels.iter().any(|l| l.name == SKIP_CHANGELOG_LABEL) {
            println!("Skipping changelog check because of '{SKIP_CHANGELOG_LABEL}' label");
            return Ok(());
        }

        println!("Labels for PR: {:?}", info.labels);

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
        let message = format!(
            "Please do not edit {CHANGELOG_FILE} directly. Take a look at [CONTRIBUTING.md](https://github.com/probe-rs/probe-rs/blob/master/CONTRIBUTING.md) for information on changelog fragments instead."
        );

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
            "No new changelog fragments detected, and '{SKIP_CHANGELOG_LABEL}' label not applied."
        );
    }

    Ok(())
}

/// `pr`: The PR number, if any, to comment on.
fn print_fragment_list(fragment_list: FragmentList, pr: Option<u64>) -> Result<()> {
    let message = fragment_list.display();
    println!("{message}");

    if !fragment_list.is_ok()
        && let Some(pr) = pr
    {
        write_comment(pr, &message)?;
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
    let shell = Shell::new()?;
    let release_date = cmd!(shell, "date +%Y-%m-%d").read()?.trim().to_string();

    writeln!(writer, "## [{version}]")?;
    writeln!(writer)?;
    writeln!(writer, "Released {release_date}")?;
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

    println!("Assembled changelog for version {version}:");
    println!("{}", String::from_utf8(assembled.clone())?);

    let old_changelong_content = std::fs::read_to_string(CHANGELOG_FILE)?;

    let mut changelog_file = std::fs::File::create(CHANGELOG_FILE)?;

    let mut content_inserted = false;

    for line in old_changelong_content.lines() {
        if !content_inserted && line.starts_with("## ") {
            changelog_file.write_all(&assembled)?;
            content_inserted = true
        }

        writeln!(changelog_file, "{line}")?;
    }

    println!("Changelog {CHANGELOG_FILE} updated.");

    if !no_cleanup {
        println!("Cleaning up fragments...");

        for fragments in fragment_list.fragments.values() {
            for fragment in fragments {
                println!(" Removing {}", fragment.path.display());
                std::fs::remove_file(&fragment.path)?;
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::{default_sync_branch_name, sanitize_branch_component};

    #[test]
    fn derives_sync_branch_from_tag() {
        assert_eq!(default_sync_branch_name("v0.30.0"), "sync/v0.30.0");
        assert_eq!(
            default_sync_branch_name("refs/tags/v0.30.0"),
            "sync/v0.30.0"
        );
    }

    #[test]
    fn derives_sync_branch_from_branch_ref() {
        assert_eq!(
            default_sync_branch_name("probe-rs/master"),
            "sync/probe-rs-master"
        );
        assert_eq!(
            default_sync_branch_name("refs/heads/release/0.30"),
            "sync/release-0.30"
        );
    }

    #[test]
    fn sanitizes_invalid_branch_characters() {
        assert_eq!(
            sanitize_branch_component(" release candidate:v0.30.0 "),
            "release-candidate-v0.30.0"
        );
        assert_eq!(sanitize_branch_component("///"), "");
    }
}

fn write_changelog_section(
    mut writer: impl std::io::Write,
    heading: &str,
    fragments: &[Fragment],
) -> anyhow::Result<()> {
    writeln!(writer, "### {heading}")?;
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

        write!(writer, "- {first_line}")?;

        // Write remaining lines
        for line in lines {
            writeln!(writer)?;
            write!(writer, "  {line}")?;
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
