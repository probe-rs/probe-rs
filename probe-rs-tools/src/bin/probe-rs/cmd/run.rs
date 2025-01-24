use std::path::{Path, PathBuf};

use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::{MonitorMode, MonitorOptions};

use crate::util::cli::{self, rtt_client};
use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::FormatOptions;

use libtest_mimic::{Arguments, FormatSetting};
use probe_rs::flashing::FileDownloadError;
use std::fs::File;
use std::io::Read;

/// Options only used in normal run mode
#[derive(Debug, clap::Parser, Clone)]
pub struct NormalRunOptions {
    /// Enable reset vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_reset: bool,
    /// Enable hardfault vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_hardfault: bool,
}

/// Options only used when in test run mode
#[derive(Debug, clap::Parser)]
pub struct TestOptions {
    /// Filter string. Only tests which contain this string are run.
    #[clap(
        index = 2,
        value_name = "TEST_FILTER",
        help = "The TEST_FILTER string is tested against the name of all tests, and only those tests whose names contain the filter are run. Multiple filter strings may be passed, which will run all tests matching any of the filters.",
        help_heading = "TEST OPTIONS"
    )]
    pub filter: Vec<String>,

    /// Only list all tests
    #[clap(
        long = "list",
        help = "List all tests instead of executing them",
        help_heading = "TEST OPTIONS"
    )]
    pub list: bool,

    #[clap(
        long = "format",
        value_enum,
        value_name = "pretty|terse|json",
        help_heading = "TEST OPTIONS",
        help = "Configure formatting of the test report output"
    )]
    pub format: Option<FormatSetting>,

    /// If set, filters are matched exactly rather than by substring.
    #[clap(long = "exact", help_heading = "TEST OPTIONS")]
    pub exact: bool,

    /// If set, run only ignored tests.
    #[clap(long = "ignored", help_heading = "TEST OPTIONS")]
    pub ignored: bool,

    /// If set, run ignored and non-ignored tests.
    #[clap(long = "include-ignored", help_heading = "TEST OPTIONS")]
    pub include_ignored: bool,

    /// A list of filters. Tests whose names contain parts of any of these
    /// filters are skipped.
    #[clap(
        long = "skip-test",
        value_name = "FILTER",
        help_heading = "TEST OPTIONS",
        help = "Skip tests whose names contain FILTER (this flag can be used multiple times)"
    )]
    pub skip_test: Vec<String>,

    /// Options which are ignored, but exist for compatibility with libtest.
    /// E.g. so that vscode and intellij can invoke the test runner with the args they are used to
    #[clap(flatten)]
    _no_op: NoOpTestOptions,
}

/// Options which are ignored, but exist for compatibility with libtest.
#[derive(Debug, clap::Parser)]
struct NoOpTestOptions {
    // No-op, ignored (libtest-mimic always runs in no-capture mode)
    #[clap(long = "nocapture", hide = true)]
    nocapture: bool,

    /// No-op, ignored. libtest-mimic does not currently capture stdout.
    #[clap(long = "show-output", hide = true)]
    show_output: bool,

    /// No-op, ignored. Flag only exists for CLI compatibility with libtest.
    #[clap(short = 'Z', hide = true)]
    unstable_flags: Option<String>,
}

#[derive(clap::Parser)]
pub struct Cmd {
    /// Options only used when in normal run mode
    #[clap(flatten)]
    pub(crate) run_options: NormalRunOptions,

    /// Options only used when in test mode
    #[clap(flatten)]
    pub(crate) test_options: TestOptions,

    /// Options shared by all run modes
    #[clap(flatten)]
    pub(crate) shared_options: SharedOptions,
}

#[derive(Debug, clap::Parser)]
pub struct SharedOptions {
    #[clap(flatten)]
    pub(crate) probe_options: ProbeOptions,

    #[clap(flatten)]
    pub(crate) download_options: BinaryDownloadOptions,

    /// The path to the ELF file to flash and run.
    #[clap(
        index = 1,
        help = "The path to the ELF file to flash and run.\n\
    If the binary uses `embedded-test` each test will be executed in turn. See `TEST OPTIONS` for more configuration options exclusive to this mode.\n\
    If the binary does not use `embedded-test` the binary will be flashed and run normally. See `RUN OPTIONS` for more configuration options exclusive to this mode."
    )]
    pub(crate) path: PathBuf,

    /// Always print the stacktrace on ctrl + c.
    #[clap(long)]
    pub(crate) always_print_stacktrace: bool,

    /// Whether to erase the entire chip before downloading
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub(crate) chip_erase: bool,

    /// Suppress filename and line number information from the rtt log
    #[clap(long)]
    pub(crate) no_location: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    /// The format string to use when printing defmt encoded log messages from the target.
    ///
    /// You can also use one of two presets: oneline (default) and full.
    ///
    /// See <https://defmt.ferrous-systems.com/custom-log-output>
    #[clap(long)]
    pub(crate) log_format: Option<String>,

    /// Scan the memory to find the RTT control block
    #[clap(long)]
    pub(crate) rtt_scan_memory: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        // Detect run mode based on ELF file
        let run_mode = detect_run_mode(&self)?;

        let session = cli::attach_probe(&client, self.shared_options.probe_options, true).await?;

        let mut rtt_client = rtt_client(
            &session,
            &self.shared_options.path,
            match self.shared_options.rtt_scan_memory {
                true => crate::rpc::functions::rtt_client::ScanRegion::TargetDefault,
                false => crate::rpc::functions::rtt_client::ScanRegion::Ranges(vec![]),
            },
            self.shared_options.log_format,
            !self.shared_options.no_location,
            None,
        )
        .await?;

        let client_handle = rtt_client.handle();

        // Flash firmware
        let boot_info = cli::flash(
            &session,
            &self.shared_options.path,
            self.shared_options.chip_erase,
            self.shared_options.format_options,
            self.shared_options.download_options,
            Some(&mut rtt_client),
        )
        .await?;

        // Run firmware based on run mode
        if run_mode == RunMode::Test {
            cli::test(
                &session,
                boot_info,
                Arguments {
                    test_threads: Some(1), // Avoid parallel execution
                    list: self.test_options.list,
                    exact: self.test_options.exact,
                    ignored: self.test_options.ignored,
                    include_ignored: self.test_options.include_ignored,
                    format: self.test_options.format,
                    skip: self.test_options.skip_test.clone(),
                    filter: if self.test_options.filter.is_empty() {
                        None
                    } else {
                        //TODO: Fix libtest-mimic so that it allows multiple filters (same as std test runners)
                        Some(self.test_options.filter.join(" "))
                    },
                    ..Arguments::default()
                },
                self.shared_options.always_print_stacktrace,
                &self.shared_options.path,
                Some(rtt_client),
            )
            .await
        } else {
            cli::monitor(
                &session,
                MonitorMode::Run(boot_info),
                &self.shared_options.path,
                Some(rtt_client),
                MonitorOptions {
                    catch_reset: self.run_options.catch_reset,
                    catch_hardfault: self.run_options.catch_hardfault,
                    rtt_client: Some(client_handle),
                },
                self.shared_options.always_print_stacktrace,
            )
            .await
        }
    }
}

#[derive(PartialEq)]
enum RunMode {
    Normal,
    Test,
}

fn elf_contains_test(path: &Path) -> anyhow::Result<bool> {
    let mut file = File::open(path).map_err(FileDownloadError::IO)?;

    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let contains = match goblin::elf::Elf::parse(buffer.as_slice()) {
        Ok(elf) if elf.syms.is_empty() => {
            tracing::debug!("No Symbols in ELF");
            false
        }
        Ok(elf) => elf
            .syms
            .iter()
            .any(|sym| elf.strtab.get_at(sym.st_name) == Some("EMBEDDED_TEST_VERSION")),
        Err(_) => {
            tracing::debug!("Failed to parse ELF file");
            false
        }
    };

    Ok(contains)
}

fn detect_run_mode(cmd: &Cmd) -> anyhow::Result<RunMode> {
    if elf_contains_test(&cmd.shared_options.path)? {
        // We tolerate the run options, even in test mode so that you can set
        // `probe-rs run --catch-hardfault` as cargo runner (used for both unit tests and normal binaries)
        tracing::info!("Detected embedded-test in ELF file. Running as test");
        Ok(RunMode::Test)
    } else {
        let test_args_specified = cmd.test_options.list
            || cmd.test_options.exact
            || cmd.test_options.format.is_some()
            || !cmd.test_options.filter.is_empty();

        if test_args_specified {
            anyhow::bail!("probe-rs was invoked with arguments exclusive to test mode, but the binary does not contain embedded-test");
        }

        tracing::debug!("No embedded-test in ELF file. Running as normal");
        Ok(RunMode::Normal)
    }
}
