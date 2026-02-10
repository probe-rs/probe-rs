use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use probe_rs::Session;

use anyhow::Context;
use object::{Object, ObjectSymbol};
mod frame_pointer;
mod fxprof;
mod samply_object;

#[derive(clap::Args, Clone, Debug, PartialEq)]
pub(crate) struct CallstackProfileArgs {
    #[clap(subcommand)]
    pub(crate) method: CallstackProfileMethod,
    /// Target sampling rate, in Hz. Higher frequencies will have a larger impact on execution and
    /// so will be less representative of true behaviour. If the rate is set too high it may not be
    /// achieved.
    #[clap(long, default_value_t = 2.)]
    pub(crate) rate: f64,
    /// Comma separated list of cores to profile, numbered from 0.
    /// If more than one is specified then the list of cores will be cycled through, with a single
    /// core sampled at each interval.
    #[clap(long, value_delimiter = ',', default_values_t = [0])]
    pub(crate) cores: Vec<usize>,
    /// Output format.
    #[clap(long, value_enum, default_value_t = OutputFormat::FirefoxProfiler)]
    pub(crate) output_format: OutputFormat,
}

#[derive(clap::Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallstackProfileMethod {
    /// Naively (halt -> walk -> resume) walk call stack using frame pointers and frame record
    /// chain. You should ensure the program was compiled with frame pointers enabled.
    /// For rust, set the codegen option force-frame-pointers=yes.
    /// For C/C++ gcc/clang, set -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer.
    NaiveFramePointer,
}

#[derive(clap::ValueEnum, Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Firefox profiler output format that can be opened using:
    /// samply load probe-rs-profile.json.gz
    FirefoxProfiler,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FunctionAddress {
    ProgramCounter(u64),
    // Return address adjusted to point to start of call instruction
    // See `fxprofpp::Frame::AdjustedReturnAddress`
    AdjustedReturnAddress(u64),
}

// Format addresses as hex for debugging
impl std::fmt::Debug for FunctionAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProgramCounter(addr) => f
                .debug_tuple("ProgramCounter")
                .field(&format!("{addr:#x}"))
                .finish(),
            Self::AdjustedReturnAddress(addr) => f
                .debug_tuple("AdjustedReturnAddress")
                .field(&format!("{addr:#x}"))
                .finish(),
        }
    }
}

/// A single sample containing a callstack and a time
#[derive(Clone, Debug, PartialEq, Eq)]
struct CallstackSample {
    // element 0 is root node
    // element 1 is first callee, etc
    callstack: Vec<FunctionAddress>,
    // time since profiling started
    time: Duration,
}

/// All callstacks collected for a given core, for interfacing different sample collection methods
/// with different output formats
#[derive(Clone, Debug)]
struct CoreSamples {
    core: usize,
    callstacks: Vec<CallstackSample>,
}

impl CoreSamples {
    fn new(core: usize) -> Self {
        Self {
            core,
            callstacks: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Could not find entry point address range")]
pub struct EntryPointAddressRangeError;

/// Find the range of addresses of the ELF's entry point function
fn get_entry_point_address_range<'data>(
    obj: &impl Object<'data>,
) -> Result<std::ops::Range<u64>, EntryPointAddressRangeError> {
    let entry_start = obj.entry();

    // find next function symbol after entry point
    let entry_end = obj
        .symbols()
        .filter(|sym| sym.kind() == object::SymbolKind::Text)
        .map(|sym| sym.address())
        .filter(|addr| *addr > entry_start)
        .min()
        .ok_or(EntryPointAddressRangeError)?;

    Ok(entry_start..entry_end)
}

pub(super) fn callstack_profile(
    session: &mut Session,
    duration: u64,
    executable_location: &Path,
    callstack_profile_args: &CallstackProfileArgs,
) -> anyhow::Result<()> {
    let duration = Duration::from_secs(duration);
    let sampling_interval = Duration::from_nanos((1e9 / callstack_profile_args.rate) as u64);

    let object_bytes = std::fs::read(executable_location)?;
    let object = object::File::parse(object_bytes.as_slice())?;
    let entry_address_range = get_entry_point_address_range(&object)?;

    let mut samples: Vec<CoreSamples> = callstack_profile_args
        .cores
        .iter()
        .map(|core_idx| CoreSamples::new(*core_idx))
        .collect();
    let mut core_index = 0;

    let start = Instant::now();
    let start_sys_time = std::time::SystemTime::now();

    loop {
        let current_sample_start = std::time::Instant::now();

        let core_sample = &mut samples[core_index];
        let mut core = session.core(core_sample.core)?;

        // collect sample
        core.halt(Duration::from_millis(10))?;
        let callstack = match callstack_profile_args.method {
            CallstackProfileMethod::NaiveFramePointer => {
                frame_pointer::frame_pointer_stack_walk(&mut core, &entry_address_range)
                    .context("Unwinding error, was the program compiled with frame pointers?")?
            }
        };
        core.run()?;

        let sample = CallstackSample {
            callstack,
            time: std::time::Instant::now().duration_since(start),
        };

        core_sample.callstacks.push(sample);

        if start.elapsed() > duration {
            break;
        }

        // repeatedly cycle through sampled cores
        core_index = (core_index + 1) % samples.len();

        // sleep a bit before next sample to try to match sampling rate
        let current_sample_time = current_sample_start.elapsed();
        std::thread::sleep(sampling_interval.saturating_sub(current_sample_time));
    }

    // output profiling data
    match callstack_profile_args.output_format {
        OutputFormat::FirefoxProfiler => {
            let profile = fxprof::make_fx_profile(
                &samples,
                &start_sys_time,
                &sampling_interval,
                executable_location,
                &object,
            )?;

            let output_dir = std::env::current_dir()?;
            let profile_name = "probe-rs-profile";
            fxprof::save_fx_profile(&profile, &output_dir, profile_name)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use super::*;

    /// Get the full path to a file in the `tests` directory.
    pub(crate) fn get_path_for_test_files(relative_file: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.push("probe-rs-debug");
        path.push("tests");
        path.push(relative_file);
        path
    }

    /// Helper to convert slice of addresses to callstack Vec
    /// Input order is callee most to caller most
    /// These can be found loading the coredump with gdb and then running bt, gdb may not adjust
    /// the addresses in the same way though:
    /// rust-gdb --se <elf-executable-file> --core <elf-format-core-file>
    /// info registers pc
    /// bt
    ///
    /// can do stack walk inside gdb:
    /// info registers fp
    /// loop:
    ///     p/x *((addr + <offset-fp>) as *u32)
    ///     p/x *((addr + <offset-ra>) as *u32)
    pub(crate) fn addresses_to_callstack(addresses: &[u64]) -> Vec<FunctionAddress> {
        addresses
            .iter()
            .copied()
            .enumerate()
            .map(|(i, val)| match i {
                0 => FunctionAddress::ProgramCounter(val),
                _ => FunctionAddress::AdjustedReturnAddress(val),
            })
            .rev()
            .collect()
    }

    #[test]
    fn test_get_entry_point_address_range() {
        let executable_name = "esp32c6_coredump_elf";
        let executable_location =
            get_path_for_test_files(format!("debug-unwind-tests/{executable_name}.elf").as_str());

        let object_bytes = std::fs::read(&executable_location).unwrap();
        let obj = object::File::parse(object_bytes.as_slice()).unwrap();

        let entry_point_address_range = get_entry_point_address_range(&obj).unwrap();

        let expect = 0x42000020..0x42000104;
        assert_eq!(entry_point_address_range, expect);
    }
}
