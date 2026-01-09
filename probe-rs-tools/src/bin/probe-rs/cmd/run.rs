use std::path::{Path, PathBuf};

use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::{MonitorMode, MonitorOptions};
use crate::rpc::functions::test::{Test, TestDefinition};

use crate::FormatOptions;
use crate::util::cli::{self, connect_target_output_files, parse_semihosting_options, rtt_client};
use crate::util::common_options::{BinaryDownloadOptions, ProbeOptions};

use anyhow::{Context, anyhow};
use goblin::elf::Elf;
use goblin::elf::SectionHeader;
use goblin::elf::Sym;
use goblin::elf::sym::{STB_GLOBAL, STT_OBJECT};
use libtest_mimic::{Arguments, FormatSetting};
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

    /// Suppress filename and line number information from the rtt log
    #[clap(long)]
    pub(crate) no_location: bool,

    /// Suppress timestamps from the rtt log
    #[clap(long)]
    pub(crate) no_timestamps: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    /// The format string to use when printing defmt encoded log messages from the target.
    ///
    /// You can also use one of two presets: oneline (default) and full.
    ///
    /// See <https://defmt.ferrous-systems.com/custom-log-output>
    #[clap(long)]
    pub(crate) log_format: Option<String>,

    /// File name to store formatted output at. Different channels can be assigned to different
    /// files using channel=file arguments to multiple occurrences (eg. `--target-output-file
    /// defmt=out/defmt.txt --target-output-file out/default`). Channel names can be prefixed with
    /// `rtt:` or `semihosting:` (eg. `semihosting:stdout`) to disambiguate.
    #[clap(long)]
    pub(crate) target_output_file: Vec<String>,

    /// Scan the memory to find the RTT control block
    #[clap(long)]
    pub(crate) rtt_scan_memory: bool,

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

        let session = cli::attach_probe(&client, self.shared_options.probe_options, false).await?;

        let mut rtt_client = rtt_client(
            &session,
            &self.shared_options.path,
            match self.shared_options.rtt_scan_memory {
                true => crate::rpc::functions::rtt_client::ScanRegion::TargetDefault,
                false => crate::rpc::functions::rtt_client::ScanRegion::Ranges(vec![]),
            },
            self.shared_options.log_format,
            !self.shared_options.no_timestamps,
            !self.shared_options.no_location,
            Some(utc_offset),
        )
        .await?;

        let mut target_output_files =
            connect_target_output_files(self.shared_options.target_output_file).await?;

        let semihosting_options = parse_semihosting_options(self.shared_options.semihosting_file)?;

        let client_handle = rtt_client.handle();

        // Flash firmware
        let boot_info = cli::flash(
            &session,
            &self.shared_options.path,
            self.shared_options.download_options.chip_erase,
            self.shared_options.format_options,
            self.shared_options.download_options,
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
                        //TODO: Fix libtest-mimic so that it allows multiple filters (same as std test runners)
                        Some(self.test_options.filter.join(" "))
                    },
                    ..Arguments::default()
                },
                self.shared_options.always_print_stacktrace,
                &self.shared_options.path,
                Some(rtt_client),
                &mut target_output_files,
                semihosting_options,
            )
            .await
        } else {
            cli::monitor(
                &session,
                MonitorMode::Run(boot_info),
                &self.shared_options.path,
                Some(rtt_client),
                MonitorOptions {
                    catch_reset: !self.run_options.no_catch_reset,
                    catch_hardfault: !self.run_options.no_catch_hardfault,
                    rtt_client: Some(client_handle),
                    semihosting_options,
                },
                self.shared_options.always_print_stacktrace,
                &mut target_output_files,
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
    elf: Elf<'a>,
}

impl<'a> ElfReader<'a> {
    fn decode(&self) -> anyhow::Result<Option<EmbeddedTestElfInfo>> {
        if self.elf.syms.is_empty() {
            tracing::debug!("No Symbols in ELF");
            return Ok(None);
        }

        // Find our custom .embedded_test section which contains version info and possibly testcases
        let Some((et_section_index, et_section)) = self
            .elf
            .section_headers
            .iter()
            .enumerate()
            .find(|(_idx, hdr)| self.elf.shdr_strtab.get_at(hdr.sh_name) == Some(".embedded_test"))
        else {
            tracing::debug!("No .embedded_test linker section in ELF");
            return Ok(None);
        };

        let Some(version_sym) = self.elf.syms.iter().find(|sym| {
            sym.st_bind() == STB_GLOBAL
                && sym.st_type() == STT_OBJECT
                && sym.st_shndx == et_section_index
                && sym.st_size == 4 // sizeof( u32 )
                && matches!(self.symbol_name_of(sym), Ok("EMBEDDED_TEST_VERSION"))
        }) else {
            tracing::debug!("No EMBEDDED_TEST_VERSION symbol in ELF");
            return Ok(None);
        };

        let version =
            self.read_u32_at_offset(self.file_offset_for(version_sym.st_value, et_section))?;

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
                for sym in self.elf.syms.iter() {
                    if sym.st_bind() == STB_GLOBAL
                        && sym.st_type() == STT_OBJECT
                        && sym.st_shndx == et_section_index
                        && sym.st_size == 12
                    // sizeof( (fn()->!, &'static str) )
                    {
                        tests.push(self.decode_testcase_sym(&sym, et_section)?);
                    }
                }

                Ok(Some(EmbeddedTestElfInfo { version, tests }))
            }

            _ => Err(anyhow!(
                "Found embedded_test protocol version {version}, which is not yet supported by probe-rs. Update probe-rs?"
            )),
        }
    }

    fn decode_testcase_sym(&self, sym: &Sym, et_section: &SectionHeader) -> anyhow::Result<Test> {
        // A testcase is stored as tuple of testfunc + module_path
        // and has type (fn()->!, &'static str) which is 12 bytes.
        // The symbol name is a escaped json object containing info about the test

        let file_offset = self.file_offset_for(sym.st_value, et_section);
        let test_fn_ptr = self.read_u32_at_offset(file_offset)?;
        let mod_path_ptr = self.read_u32_at_offset(file_offset + 4)?;
        let mod_path_len = self.read_u32_at_offset(file_offset + 8)?;

        let mod_path = self.read_mod_path(mod_path_ptr, mod_path_len)?;
        let sym_name = self.symbol_name_of(sym)?;
        let def: TestDefinition = serde_json::from_str(sym_name)?;
        let mut test: Test = def.into();
        test.name = format!("{mod_path}::{}", test.name); //prepend mod path to test name
        test.address = Some(test_fn_ptr);
        Ok(test)
    }

    fn symbol_name_of(&self, sym: &Sym) -> anyhow::Result<&'a str> {
        self.elf
            .strtab
            .get_at(sym.st_name)
            .ok_or(anyhow!("No name for symbol {sym:?}"))
    }

    #[inline]
    fn file_offset_for(&self, addr: u64, section: &SectionHeader) -> usize {
        (section.sh_offset + (addr - section.sh_addr)) as usize
    }

    #[inline]
    fn read_u32_at_offset(&self, file_offset: usize) -> anyhow::Result<u32> {
        Ok(u32::from_le_bytes(
            self.buffer[file_offset..file_offset + 4].try_into()?,
        ))
    }

    fn read_mod_path(&self, mod_path_ptr: u32, mod_path_len: u32) -> anyhow::Result<&'a str> {
        let section = self
            .elf
            .section_headers
            .iter()
            .find(|section| {
                mod_path_ptr >= section.sh_addr as u32
                    && mod_path_ptr + mod_path_len <= (section.sh_addr + section.sh_size) as u32
            })
            .ok_or(anyhow!(
                "section not found for mod path str {mod_path_ptr:x}"
            ))?;

        let file_offset = self.file_offset_for(mod_path_ptr as u64, section);
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

        let elf = goblin::elf::Elf::parse(buffer).context("Failed to parse ELF file")?;

        ElfReader { buffer, elf }
            .decode()
            .context("Failed to read embedded_test testcases from provided ELF")
    }
}

fn detect_run_mode(cmd: &Cmd) -> anyhow::Result<RunMode> {
    if let Some(elf_info) = EmbeddedTestElfInfo::from_elf(&cmd.shared_options.path)? {
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
