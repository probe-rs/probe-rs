use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use clap::Parser;
use regex::Regex;
use serde_json::Value;
use xshell::{Shell, cmd};
use sha2::{Sha256, Digest};
use chrono::Utc;

use anyhow::Result;
use probe_rs_crc32_builder::crc_config;

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
    /// Build CRC32C embedded binaries for multiple architectures
    BuildCrc32 {
        /// Build only ARM targets (equivalent to build.sh)
        #[arg(long)]
        arm_only: bool,
        /// Build all architecture variants (includes redundant binaries)
        #[arg(long)]
        all_variants: bool,
        /// Skip toolchain availability checks and build all possible targets
        #[arg(long)]
        force: bool,
    },
    /// Clean CRC32 embedded binaries and build artifacts
    CleanCrc32,
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
        Cli::BuildCrc32 { arm_only, all_variants, force } => build_crc32(arm_only, all_variants, force)?,
        Cli::CleanCrc32 => clean_crc32()?,
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

fn create_release_pr(version: String) -> Result<()> {
    let sh = Shell::new()?;

    // Make sure we are on the right branch and we have the latest state pulled from our source of truth, GH.

    let branch = cmd!(sh, "git branch --show-current")
        .read()?
        .trim()
        .to_string();

    if branch != "master" && !branch.starts_with("release/") {
        bail!(
            "Invalid current branch '{branch}'. Make sure you're either on `master` or `release/x.y`."
        )
    }

    cmd!(
        sh,
        "gh workflow run 'Open a release PR' --ref {branch} -f version={version}"
    )
    .run()?;

    Ok(())
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

        write!(writer, " - {first_line}")?;

        // Write remaining lines
        for line in lines {
            writeln!(writer)?;
            write!(writer, "   {line}")?;
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

// CRC32C Build System Implementation
// Replaces build.sh and build-multi-arch.sh with cargo-based automation

#[derive(Debug)]
struct TargetInfo {
    name: String,
    description: String,
    objcopy_tool: String,
    objdump_tool: String,
    architecture: String,
}

fn build_crc32(arm_only: bool, all_variants: bool, force: bool) -> Result<()> {
    let sh = Shell::new()?;

    // Change directory to crc32_algorithms
    let crc32_dir = Path::new("crc32_algorithms");
    if !crc32_dir.exists() {
        bail!("crc32_algorithms directory not found. Please run this command from the probe-rs root directory.");
    }

    let _guard = sh.push_dir(crc32_dir);

    println!("🔧 Building CRC32C embedded binaries...");
    println!();

    // Check dependencies
    check_dependencies(&sh, arm_only, force)?;

    // Define targets based on mode
    let targets = if arm_only {
        get_arm_targets()
    } else if all_variants {
        get_all_targets(force)?
    } else {
        // Default to minimal universal set
        get_minimal_targets(force)?
    };

    if targets.is_empty() {
        bail!("No targets available to build. Install required toolchains or use --force.");
    }

    // Ensure Cargo.toml exists
    ensure_cargo_toml_exists()?;

    // Build each target
    let mut built_files = Vec::new();
    let mut failed_targets = Vec::new();

    for target in &targets {
        match build_target(&sh, target) {
            Ok(files) => {
                built_files.extend(files);
                println!("✅ {}: SUCCESS", target.name);
            }
            Err(e) => {
                eprintln!("❌ {}: FAILED - {}", target.name, e);
                failed_targets.push(&target.name);
            }
        }
        println!();
    }

    // Summary
    print_build_summary(&built_files, &failed_targets)?;

    if !failed_targets.is_empty() && !force {
        bail!("Some targets failed to build. Use --force to ignore toolchain issues.");
    }

    Ok(())
}

fn check_dependencies(sh: &Shell, arm_only: bool, force: bool) -> Result<()> {
    println!("🔍 Checking build dependencies...");

    // Check cargo
    if cmd!(sh, "cargo --version").run().is_err() {
        bail!("cargo not found. Please install Rust toolchain.");
    }

    // Check architecture-specific tools
    let mut missing_tools = Vec::new();

    if cmd!(sh, "arm-none-eabi-objcopy --version").run().is_err() {
        missing_tools.push("ARM toolchain (arm-none-eabi-objcopy)");
    }

    if !arm_only {
        if cmd!(sh, "riscv64-unknown-elf-objcopy --version").run().is_err() {
            missing_tools.push("RISC-V toolchain (riscv64-unknown-elf-objcopy)");
        }
    }

    if !missing_tools.is_empty() && !force {
        println!("❌ Missing toolchains:");
        for tool in &missing_tools {
            println!("  - {}", tool);
        }
        println!();
        println!("Install missing toolchains or use --force to skip unavailable targets.");
        println!("ARM: sudo apt install gcc-arm-none-eabi (Ubuntu) or brew install gcc-arm-embedded (macOS)");
        println!("RISC-V: Install riscv64-unknown-elf-gcc toolchain");
        bail!("Missing required toolchains");
    }

    if !missing_tools.is_empty() {
        println!("⚠️  Missing toolchains (will skip): {:?}", missing_tools);
    } else {
        println!("✅ All required toolchains available");
    }

    println!();
    Ok(())
}

fn get_arm_targets() -> Vec<TargetInfo> {
    vec![TargetInfo {
        name: "thumbv6m-none-eabi".to_string(),
        description: "Universal ARM Cortex-M (M0/M0+/M3/M4/M7)".to_string(),
        objcopy_tool: "arm-none-eabi-objcopy".to_string(),
        objdump_tool: "arm-none-eabi-objdump".to_string(),
        architecture: "ARM".to_string(),
    }]
}

fn get_minimal_targets(force: bool) -> Result<Vec<TargetInfo>> {
    let sh = Shell::new()?;
    let mut targets = Vec::new();

    // Always include universal ARM target
    if force || cmd!(sh, "arm-none-eabi-objcopy --version").run().is_ok() {
        targets.push(TargetInfo {
            name: "thumbv6m-none-eabi".to_string(),
            description: "Universal ARM Cortex-M (all variants)".to_string(),
            objcopy_tool: "arm-none-eabi-objcopy".to_string(),
            objdump_tool: "arm-none-eabi-objdump".to_string(),
            architecture: "ARM".to_string(),
        });
    }

    // Include minimal RISC-V set for universal compatibility
    if force || cmd!(sh, "riscv64-unknown-elf-objcopy --version").run().is_ok() {
        targets.extend(vec![
            TargetInfo {
                name: "riscv32i-unknown-none-elf".to_string(),
                description: "Universal RISC-V RV32I (base - works on all RV32 cores)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
            TargetInfo {
                name: "riscv32imc-unknown-none-elf".to_string(),
                description: "RISC-V RV32IMC (compressed - optimal for most modern cores)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
        ]);
    }

    Ok(targets)
}

fn get_all_targets(force: bool) -> Result<Vec<TargetInfo>> {
    let sh = Shell::new()?;
    let mut targets = Vec::new();

    // ARM targets (if toolchain available) - all variants for compatibility testing
    if force || cmd!(sh, "arm-none-eabi-objcopy --version").run().is_ok() {
        targets.extend(vec![
            TargetInfo {
                name: "thumbv6m-none-eabi".to_string(),
                description: "ARM Cortex-M0/M0+ (Universal compatibility)".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
            TargetInfo {
                name: "thumbv7m-none-eabi".to_string(),
                description: "ARM Cortex-M3".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
            TargetInfo {
                name: "thumbv7em-none-eabi".to_string(),
                description: "ARM Cortex-M4/M7 (no FPU)".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
            TargetInfo {
                name: "thumbv7em-none-eabihf".to_string(),
                description: "ARM Cortex-M4/M7 (with FPU)".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
            TargetInfo {
                name: "thumbv8m.base-none-eabi".to_string(),
                description: "ARM Cortex-M23".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
            TargetInfo {
                name: "thumbv8m.main-none-eabi".to_string(),
                description: "ARM Cortex-M33/M55".to_string(),
                objcopy_tool: "arm-none-eabi-objcopy".to_string(),
                objdump_tool: "arm-none-eabi-objdump".to_string(),
                architecture: "ARM".to_string(),
            },
        ]);
    }

    // RISC-V targets (if toolchain available) - all variants for compatibility testing
    if force || cmd!(sh, "riscv64-unknown-elf-objcopy --version").run().is_ok() {
        targets.extend(vec![
            TargetInfo {
                name: "riscv32i-unknown-none-elf".to_string(),
                description: "RISC-V RV32I (base integer)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
            TargetInfo {
                name: "riscv32im-unknown-none-elf".to_string(),
                description: "RISC-V RV32IM (integer + multiplication)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
            TargetInfo {
                name: "riscv32imc-unknown-none-elf".to_string(),
                description: "RISC-V RV32IMC (ESP32-C3 compatible)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
            TargetInfo {
                name: "riscv32imac-unknown-none-elf".to_string(),
                description: "RISC-V RV32IMAC (ESP32-C6/H2 compatible)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
            TargetInfo {
                name: "riscv64imac-unknown-none-elf".to_string(),
                description: "RISC-V RV64IMAC (64-bit)".to_string(),
                objcopy_tool: "riscv64-unknown-elf-objcopy".to_string(),
                objdump_tool: "riscv64-unknown-elf-objdump".to_string(),
                architecture: "RISC-V".to_string(),
            },
        ]);
    }

    Ok(targets)
}

fn ensure_cargo_toml_exists() -> Result<()> {
    if Path::new("Cargo.toml").exists() {
        return Ok(());
    }

    println!("📝 Creating Cargo.toml...");

    let cargo_toml_content = r#"[package]
name = "probe-rs-crc32-multi-arch"
version = "0.1.0"
edition = "2021"

[workspace]

[[bin]]
name = "crc32_firmware_crcxx"
path = "src/bin/firmware_crcxx.rs"

[dependencies]
crcxx = { version = "0.3", default-features = false }

[profile.release]
opt-level = "s"           # Optimize for size
lto = true                # Link-time optimization
codegen-units = 1         # Single codegen unit for better optimization
panic = "abort"           # Smaller code size
strip = false             # Keep symbols for debugging
overflow-checks = false   # Disable for smaller/faster code
"#;

    fs::write("Cargo.toml", cargo_toml_content)
        .with_context(|| "Failed to create Cargo.toml")?;

    println!("✅ Cargo.toml created");
    Ok(())
}

fn build_target(sh: &Shell, target: &TargetInfo) -> Result<Vec<String>> {
    println!("🔨 Building {}", target.name);
    println!("   Description: {}", target.description);

    // Check if target is installed
    let installed_targets = cmd!(sh, "rustup target list --installed").read()?;
    if !installed_targets.contains(&target.name) {
        println!("   Installing Rust target...");
        let target_name = &target.name;
        cmd!(sh, "rustup target add {target_name}").run()
            .with_context(|| format!("Failed to install target {}", target.name))?;
    }

    // Build the binary
    println!("   Compiling...");
    let target_name = &target.name;
    
    // Set environment variables for embedded build
    let mut cmd = cmd!(sh, "cargo build --release --target {target_name} --bin crc32_firmware_crcxx --manifest-path=Cargo.toml");
    cmd = cmd.env("RUSTFLAGS", "-C link-arg=-Tlink_minimal.x -C panic=abort");
    cmd.run()
        .with_context(|| format!("Failed to compile for target {}", target.name))?;

    // Extract binary from ELF
    println!("   Extracting binary...");
    let elf_file = format!("target/{}/release/crc32_firmware_crcxx", target.name);
    let bin_file = format!("{}.bin", target.name);
    let objcopy_tool = &target.objcopy_tool;

    cmd!(sh, "{objcopy_tool} -O binary {elf_file} {bin_file}")
        .run()
        .with_context(|| format!("Failed to extract binary for target {}", target.name))?;

    // Get function offset from ELF file 
    let function_offset = extract_function_offset(&elf_file, &target.objdump_tool)
        .with_context(|| format!("Failed to extract function offset for target {}", target.name))?;

    // Get binary size and checksum
    let binary_full_path = Path::new("crc32_algorithms").join(&bin_file);
    let binary_data = fs::read(&binary_full_path)
        .with_context(|| format!("Failed to read binary file {}", binary_full_path.display()))?;
    
    let size = binary_data.len();
    let mut hasher = Sha256::new();
    hasher.update(&binary_data);
    let checksum = format!("{:x}", hasher.finalize());

    println!("   Generated: {} ({} bytes)", bin_file, size);
    println!("   SHA256: {}...", &checksum[..16]);

    // Generate metadata TOML file
    let toml_file = format!("{}.toml", target.name);
    let toml_full_path = Path::new("crc32_algorithms").join(&toml_file);
    generate_metadata_toml(&toml_full_path, target, size, &checksum, &function_offset)?;
    println!("   Metadata: {}", toml_file);

    Ok(vec![bin_file, toml_file])
}

/// Extract the offset of the calculate_crc32 function from the ELF file
fn extract_function_offset(elf_file: &str, objdump_tool: &str) -> Result<String> {
    let sh = Shell::new()?;
    
    // Use objdump to get symbol table and find calculate_crc32 function
    let output = cmd!(sh, "{objdump_tool} -t {elf_file}")
        .read()
        .with_context(|| format!("Failed to run objdump on {}", elf_file))?;
    
    // Look for the calculate_crc32 function in the symbol table
    for line in output.lines() {
        if line.contains("calculate_crc32") && line.contains(" F ") {
            // Parse the address from the objdump output
            // Format: "00000008 g     F .text	00000030 calculate_crc32"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if !parts.is_empty() {
                let address = parts[0];
                // Validate it's a valid hex address
                if address.len() == 8 && u32::from_str_radix(address, 16).is_ok() {
                    return Ok(format!("0x{}", address));
                }
            }
        }
    }
    
    bail!("Could not find calculate_crc32 function in symbol table");
}

fn generate_metadata_toml(
    toml_path: &Path,
    target: &TargetInfo,
    size: usize,
    checksum: &str,
    function_offset: &str,
) -> Result<()> {
    let metadata_content = format!(
        r#"# CRC32C Binary Metadata for {} ({})
# Generated automatically by cargo xtask build-crc32

[binary]
target = "{}"
architecture = "{}"
size_bytes = {}
crc32_function_offset = "{}"
algorithm = "{}"
library = "crcxx v0.3.1"
entry_point = "calculate_crc32"
description = "{}"

[build_info]
generated_by = "cargo xtask build-crc32"
rust_target = "{}"
compiler_flags = "-C relocation-model=pic -C code-model=small -C panic=abort"
linker_script = "link_minimal.x"
build_date = "{}"
source_file = "src/bin/firmware_crcxx.rs"
sha256 = "{}"
objcopy_tool = "{}"
"#,
        target.name,
        target.architecture,
        target.name,
        target.architecture,
        size,
        function_offset,
        crc_config::CRC_ALGORITHM_NAME,
        target.description,
        target.name,
        Utc::now().format("%Y-%m-%d"),
        checksum,
        target.objcopy_tool
    );

    fs::write(toml_path, metadata_content)
        .with_context(|| format!("Failed to write metadata file {}", toml_path.display()))?;

    Ok(())
}

fn print_build_summary(built_files: &[String], failed_targets: &[&String]) -> Result<()> {
    println!("📋 Build Summary");
    println!("═══════════════");
    println!();

    if !built_files.is_empty() {
        println!("✅ Successfully generated files:");
        for file in built_files {
            println!("  - {}", file);
        }
        println!();
        println!("📂 All files are in the crc32_algorithms/ directory");
        println!();
    }

    if !failed_targets.is_empty() {
        println!("❌ Failed targets:");
        for target in failed_targets {
            println!("  - {}", target);
        }
        println!();
    }

    println!("🔧 Usage Instructions:");
    println!("To use these binaries:");
    println!("  1. The probe-rs flash system automatically selects the appropriate binary");
    println!("  2. ARM: thumbv6m-none-eabi.bin provides universal compatibility for ALL ARM Cortex-M variants");
    println!("  3. RISC-V: Two universal binaries provide complete coverage:");
    println!("      - riscv32i-unknown-none-elf.bin: Works on ALL RV32 cores (base ISA)");
    println!("      - riscv32imc-unknown-none-elf.bin: Optimal for modern cores with compressed instructions");
    println!();
    println!("📝 Build Options:");
    println!("  • cargo xtask build-crc32           : Build minimal universal set (default, recommended)");
    println!("  • cargo xtask build-crc32 --arm-only: Build only ARM target");
    println!("  • cargo xtask build-crc32 --all-variants: Build all architecture variants");
    println!();

    if built_files.is_empty() {
        bail!("No binaries were successfully built.");
    }

    Ok(())
}

fn clean_crc32() -> Result<()> {
    let sh = Shell::new()?;

    println!("🧹 Cleaning CRC32 embedded binaries and build artifacts...");
    println!();

    // Change directory to crc32_algorithms
    let crc32_dir = Path::new("crc32_algorithms");
    if !crc32_dir.exists() {
        bail!("crc32_algorithms directory not found. Please run this command from the probe-rs root directory.");
    }

    let _guard = sh.push_dir(crc32_dir);

    println!("🔍 Cleaning build artifacts...");
    
    // Clean Cargo build artifacts
    if Path::new("target").exists() {
        println!("  - Removing target/ directory");
        cmd!(sh, "cargo clean").run()?;
        std::fs::remove_dir_all("target").ok(); // Force remove in case cargo clean doesn't work
    }

    // Remove generated binary files
    let mut cleaned_files = Vec::new();
    
    // Remove generated binary files from crc32_algorithms directory
    if let Ok(entries) = std::fs::read_dir(crc32_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    // Check if it's a CRC binary or metadata file
                    let is_target_file = (filename.ends_with(".bin") || filename.ends_with(".toml"))
                        && (filename.starts_with("thumbv6m-none-eabi") || filename.starts_with("riscv32"));
                    
                    if is_target_file {
                        println!("  - Removing {}", filename);
                        std::fs::remove_file(&path)
                            .with_context(|| format!("Failed to remove {}", path.display()))?;
                        cleaned_files.push(filename.to_string());
                    }
                }
            }
        }
    }

    println!();
    println!("📋 Clean Summary");
    println!("════════════════");
    
    if cleaned_files.is_empty() {
        println!("✨ Already clean - no CRC32 binaries found");
    } else {
        println!("✅ Cleaned {} files:", cleaned_files.len());
        for file in &cleaned_files {
            println!("  - {}", file);
        }
    }
    
    println!();
    println!("🔧 To rebuild binaries:");
    println!("  cargo xtask build-crc32           # Build minimal universal set");
    println!("  cargo xtask build-crc32 --arm-only # Build only ARM target");
    
    Ok(())
}
