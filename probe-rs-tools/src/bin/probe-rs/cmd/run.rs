use std::path::{Path, PathBuf};

use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::MonitorMode;
use crate::rpc::functions::rtt_client::ScanRegion;
use crate::rpc::functions::test::{Test, TestDefinition};
use crate::rpc::utils::run_loop::VectorCatchConfig;

use crate::FormatOptions;
use crate::util::cli::{self, rtt_client};
use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};
use crate::util::rtt::ChannelMode;

use anyhow::{Context, anyhow};
use libtest_mimic::{Arguments, FormatSetting};
use object::{Object, ObjectSection, ObjectSymbol, Section, Symbol, SymbolKind};
use probe_rs::flashing::FileDownloadError;
use std::fs::File;
use std::io::Read;
use time::UtcOffset;

/// Options only used in normal run mode
#[derive(Debug, clap::Parser, Clone)]
pub struct NormalRunOptions {
    /// Deprecated(catch_reset is enabled by default) - Use no_reset_catch to disable this
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_reset: bool,
    /// Deprecated(catch_hardfault is enabled by default) - Use no_catch_hardfault to disable this
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_hardfault: bool,
    /// Disable reset vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub no_catch_reset: bool,
    /// Disable hardfault vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub no_catch_hardfault: bool,
    /// Disable SVC vector catch (halts on SVC exception).
    /// Only applies to ARMv7-A/R cores.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub no_catch_svc: bool,
    /// Disable HLT vector catch (halts on UNDEF exception for HLT instruction).
    /// Only applies to ARMv7-A/R cores.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub no_catch_hlt: bool,
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

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    #[clap(flatten)]
    pub(crate) monitor_options: MonitoringOptions,
}

#[derive(Debug, Clone, clap::Args)]
pub(crate) struct MonitoringOptions {
    /// The format string to use when printing defmt encoded log messages from the target.
    ///
    /// You can also use one of two presets: oneline (default) and full.
    ///
    /// See <https://defmt.ferrous-systems.com/custom-log-output>
    #[clap(long, help_heading = "LOG CONFIGURATION")]
    pub(crate) log_format: Option<String>,

    /// File name to store formatted output at. Different channels can be assigned to different
    /// files using channel=file arguments to multiple occurrences (eg. `--target-output-file
    /// defmt=out/defmt.txt --target-output-file out/default`). Channel names can be prefixed with
    /// `rtt:` or `semihosting:` (eg. `semihosting:stdout`) to disambiguate.
    #[clap(long, help_heading = "LOG CONFIGURATION")]
    pub(crate) target_output_file: Vec<String>,

    /// Memory region to scan for control block.
    ///
    /// If probe-rs finds the exact location in the binary, that location will be used. If probe-rs does not find the exact location,
    /// it will scan the specified region for the control block.
    ///
    /// You can specify either 'ram' to scan the whole memory, an exact starting address '0x1000' or a range such as '0x0000..0x1000'. Both decimal and hex are accepted.
    ///
    /// If no region is specified, probe-rs will not scan and will not poll RTT.
    #[clap(long, default_value = "", value_parser = parse_scan_region, help_heading = "LOG CONFIGURATION / RTT")]
    pub(crate) scan_region: ScanRegion,

    /// RTT channel mode to use.
    ///
    /// By default, probe-rs will configure RTT to block when the buffer is full, to avoid losing data. This option can override that behavior.
    #[clap(
        long,
        default_value = "block-if-full",
        help_heading = "LOG CONFIGURATION / RTT"
    )]
    pub(crate) rtt_channel_mode: ChannelMode,

    /// RTT up channels to display.
    ///
    /// By default, probe-rs will read and display data from all available up channels. This option can override that behavior.
    #[clap(long, help_heading = "LOG CONFIGURATION / RTT")]
    pub(crate) rtt_up_channels: Vec<u32>,

    /// RTT down channel to use.
    ///
    /// By default, probe-rs will select the first available channel. This option can override that behavior.
    #[clap(long, default_value = "0", help_heading = "LOG CONFIGURATION / RTT")]
    pub(crate) rtt_down_channel: u32,

    /// List RTT channels and exit.
    #[clap(
        long,
        default_value = "false",
        help_heading = "LOG CONFIGURATION / RTT"
    )]
    pub(crate) list_rtt: bool,

    /// Always print the stacktrace on ctrl + c.
    #[clap(long, help_heading = "LOG CONFIGURATION / STACK TRACE")]
    pub(crate) always_print_stacktrace: bool,

    /// Limit the number of stack frames to print.
    #[clap(
        long,
        default_value = "500",
        help_heading = "LOG CONFIGURATION / STACK TRACE"
    )]
    pub(crate) stack_frame_limit: u32,

    /// Suppress filename and line number information
    #[clap(long, help_heading = "LOG CONFIGURATION")]
    pub(crate) no_location: bool,

    /// Suppress timestamps
    #[clap(long, help_heading = "LOG CONFIGURATION")]
    pub(crate) no_timestamps: bool,

    /// File name to expose via semihosting. Values ending with a slash expose the whole directory.
    /// By using `target=host` arguments the names can differ between the host and the target.
    /// TCP and UNIX domain socket connections are possible by exposing files of the form
    /// `tcp:hostname:port` or `unix:/some/path`. `file:/some/path` is valid for files too.
    /// If the target path starts with a `^` and ends with a `$` it's interpreted as a regular
    /// expression and captures are expanded in the host path (e.g. `--semihosting-file
    /// "^/(\d).(\d)$=/path$1/file$2.txt"`).
    #[arg(long, help_heading = "SEMIHOSTING CONFIGURATION")]
    pub semihosting_file: Vec<String>,
}

impl Cmd {
    pub async fn run(self, client: RpcClient, utc_offset: UtcOffset) -> anyhow::Result<()> {
        // Detect run mode based on ELF file
        let run_mode = detect_run_mode(&self)?;

        // TODO: Skip attach_probe & flashing, if user only wants to list tests (only possible when using embedded_test with protocol version >= 1)

        let session = cli::attach_probe(&client, self.probe_options, false).await?;

        let mut rtt_client = rtt_client(
            &session,
            Some(&self.path),
            &self.monitor_options,
            Some(utc_offset),
        )
        .await?;

        // Flash firmware
        let boot_info = cli::flash(
            &session,
            &self.path,
            self.format_options,
            self.download_options,
            Some(&mut rtt_client),
            None,
        )
        .await?;

        // Run firmware based on run mode
        if let RunMode::Test(elf_info) = run_mode {
            cli::test(
                &session,
                boot_info,
                elf_info,
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
                        // TODO: Fix libtest-mimic so that it allows multiple filters (same as std test runners)
                        Some(self.test_options.filter.join(" "))
                    },
                    ..Arguments::default()
                },
                &self.monitor_options,
                &self.path,
                Some(rtt_client),
            )
            .await
        } else {
            cli::monitor(
                &session,
                MonitorMode::Run(boot_info),
                Some(&self.path),
                &self.monitor_options,
                Some(rtt_client),
                VectorCatchConfig {
                    catch_hardfault: !self.run_options.no_catch_hardfault,
                    catch_reset: !self.run_options.no_catch_reset,
                    catch_svc: !self.run_options.no_catch_svc,
                    catch_hlt: !self.run_options.no_catch_hlt,
                },
            )
            .await
        }
    }
}

#[derive(PartialEq)]
enum RunMode {
    Normal,
    Test(EmbeddedTestElfInfo),
}

#[derive(Debug, PartialEq)]
pub struct EmbeddedTestElfInfo {
    /// Protocol version used between embedded-test (on the target) and probe-rs
    pub version: u32,
    /// Tests found in the elf.
    pub tests: Vec<Test>,
}

struct ElfReader<'a> {
    buffer: &'a [u8],
    elf: object::File<'a>,
}

impl<'a> ElfReader<'a> {
    fn decode(&self) -> anyhow::Result<Option<EmbeddedTestElfInfo>> {
        if self.elf.symbols().next().is_none() {
            tracing::debug!("No Symbols in ELF");
            return Ok(None);
        }

        // Find our custom .embedded_test section which contains version info and possibly testcases
        let Some(et_section) = self.elf.section_by_name(".embedded_test") else {
            tracing::debug!("No .embedded_test linker section in ELF");
            return Ok(None);
        };

        let Some(version_sym) = self.elf.symbol_by_name("EMBEDDED_TEST_VERSION") else {
            tracing::debug!("No EMBEDDED_TEST_VERSION symbol in ELF");
            return Ok(None);
        };

        let version = et_section
            .data_range(version_sym.address(), version_sym.size())
            .context("Failed to read the embedded-test version symbol")?
            .unwrap();
        let version = u32::from_le_bytes(version.try_into().expect("Version must be 4 bytes"));

        match version {
            0 => {
                // In embedded test < 0.7, we have to query the tests from the target via semihosting
                Ok(Some(EmbeddedTestElfInfo {
                    version,
                    tests: vec![],
                }))
            }

            1 => {
                // Read testcases from symbols
                let mut tests = vec![];

                for sym in self.elf.symbols() {
                    if let Some(sym) = self.try_decode_testcase_sym(&sym, &et_section)? {
                        tests.push(sym);
                    }
                }

                Ok(Some(EmbeddedTestElfInfo { version, tests }))
            }

            _ => Err(anyhow!(
                "Found embedded_test protocol version {version}, which is not yet supported by probe-rs. Update probe-rs?"
            )),
        }
    }

    /// Attempts to decode a symbol as a testcase.
    ///
    /// A testcase is stored as tuple of testfunc + module_path
    /// and has type `(fn()->!, &'static str)` which is 12 bytes.
    /// The symbol name is a escaped json object containing info about the test
    fn try_decode_testcase_sym(
        &self,
        sym: &Symbol<'_, '_>,
        et_section: &Section<'_, '_>,
    ) -> anyhow::Result<Option<Test>> {
        const TESTCASE_SYM_SIZE: u64 = 12;
        if !sym.is_global()
            || sym.kind() != SymbolKind::Data
            || sym.section_index() != Some(et_section.index())
            || sym.size() != TESTCASE_SYM_SIZE
        // sizeof( (fn()->!, &'static str) )
        {
            return Ok(None);
        }

        let sym_data = et_section
            .data_range(sym.address(), TESTCASE_SYM_SIZE)?
            .unwrap();

        // Unwrap is okay, this function is only called when the symbol size is known to be 12 bytes.
        let test_fn_ptr = u32::from_le_bytes(sym_data[0..4].try_into().unwrap());
        let mod_path_ptr = u32::from_le_bytes(sym_data[4..8].try_into().unwrap());
        let mod_path_len = u32::from_le_bytes(sym_data[8..12].try_into().unwrap());

        let mod_path = self.read_mod_path(mod_path_ptr, mod_path_len)?;
        let sym_name = sym.name()?;
        let def: TestDefinition = serde_json::from_str(sym_name)?;

        let mut test: Test = def.into();
        test.name = format!("{mod_path}::{}", test.name); //prepend mod path to test name
        test.address = Some(test_fn_ptr);
        Ok(Some(test))
    }

    #[inline]
    fn file_offset_for(&self, addr: u64, section: &Section<'_, '_>) -> usize {
        let (start, _end) = section.file_range().unwrap();
        let offset = addr - section.address();
        (start + offset) as usize
    }

    fn read_mod_path(&self, mod_path_ptr: u32, mod_path_len: u32) -> anyhow::Result<&'a str> {
        let section = self
            .elf
            .sections()
            .find(|section| {
                mod_path_ptr as u64 >= section.address()
                    && mod_path_ptr as u64 + mod_path_len as u64
                        <= (section.address() + section.size())
            })
            .with_context(|| format!("section not found for mod path str {mod_path_ptr:x}"))?;

        let file_offset = self.file_offset_for(mod_path_ptr as u64, &section);
        let full_path = &self.buffer[file_offset..file_offset + mod_path_len as usize];
        let full_path = str::from_utf8(full_path)?;
        let first_col = full_path
            .find("::")
            .ok_or(anyhow!("Module path does not contain '::'"))?;
        Ok(&full_path[first_col + 2..]) // strip the crate name from the module path
    }
}

impl EmbeddedTestElfInfo {
    pub(crate) fn from_elf(path: &Path) -> anyhow::Result<Option<Self>> {
        let mut file = File::open(path).map_err(FileDownloadError::IO)?;

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        let buffer = buffer.as_slice();

        let elf = object::File::parse(buffer).context("Failed to parse ELF file")?;

        ElfReader { buffer, elf }
            .decode()
            .context("Failed to read embedded_test testcases from provided ELF")
    }
}

fn detect_run_mode(cmd: &Cmd) -> anyhow::Result<RunMode> {
    if let Some(elf_info) = EmbeddedTestElfInfo::from_elf(&cmd.path)? {
        // We tolerate the run options, even in test mode so that you can set
        // `probe-rs run --catch-hardfault` as cargo runner (used for both unit tests and normal binaries)
        tracing::info!("Detected embedded-test in ELF file. Running as test");
        tracing::debug!("Embedded Test Metadata: {:?}", elf_info);
        Ok(RunMode::Test(elf_info))
    } else {
        let test_args_specified = cmd.test_options.list
            || cmd.test_options.exact
            || cmd.test_options.format.is_some()
            || !cmd.test_options.filter.is_empty();

        if test_args_specified {
            anyhow::bail!(
                "probe-rs was invoked with arguments exclusive to test mode, but the binary does not contain embedded-test"
            );
        }

        tracing::debug!("No embedded-test in ELF file. Running as normal");
        Ok(RunMode::Normal)
    }
}

fn parse_scan_region(mut src: &str) -> anyhow::Result<ScanRegion> {
    src = src.trim();
    if src.is_empty() {
        return Ok(ScanRegion::Ranges(vec![]));
    }

    if src.eq_ignore_ascii_case("ram") {
        return Ok(ScanRegion::Ram);
    }

    let parts = src
        .split("..")
        .map(parse_int::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;

    match *parts.as_slice() {
        [addr] => Ok(ScanRegion::Exact(addr)),
        [start, end] => Ok(ScanRegion::Ranges(vec![(start, end)])),
        _ => anyhow::bail!("Invalid range: multiple '..'s"),
    }
}
