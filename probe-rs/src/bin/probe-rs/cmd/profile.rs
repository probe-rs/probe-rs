use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use probe_rs::flashing::{FileDownloadError, Format};
use time::Instant;

use addr2line::{
    gimli::{EndianRcSlice, RunTimeEndian},
    object::{read::File as ObjectFile, Object},
    Context as ObjectContext, LookupResult,
};

use crate::util::common_options::{CargoOptions, FlashOptions};
use crate::util::flash::run_flash_download;
use tracing::info;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    run: super::run::Cmd,
    /// Flash the ELF before profiling
    #[clap(long)]
    flash: bool,
    /// Print file and line info for each entry
    #[clap(long)]
    line_info: bool,
    /// Duration of profile in seconds.
    #[clap(long)]
    duration: u64, // Option<u64> If we could catch ctrl-c we can make this optional
    /// Which core to profile
    #[clap(long, default_value_t = 0)]
    core: usize,
    /// Limit the number of entries to output
    #[clap(long, default_value_t = 25)]
    limit: usize,
    #[clap(long, default_value_t = ProfileMethod::Naive)]
    /// Profile Method
    method: ProfileMethod,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileMethod {
    /// Naive, Halt -> Read PC -> Resume profiler
    Naive,
}

impl core::fmt::Display for ProfileMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let s = format!("{:?}", self);
        write!(f, "{}", s.to_lowercase())
    }
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let mut session = self.run.common.simple_attach()?;

        let mut file = match File::open(&self.run.path) {
            Ok(file) => file,
            Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
        };

        let mut loader = session.target().flash_loader();

        let format = self.run.format_options.into_format()?;
        match format {
            Format::Bin(options) => loader.load_bin_data(&mut file, options),
            Format::Elf => loader.load_elf_data(&mut file),
            Format::Hex => loader.load_hex_data(&mut file),
            Format::Idf(options) => loader.load_idf_data(&mut session, &mut file, options),
        }?;

        let bytes = std::fs::read(&self.run.path)?;
        let symbols = Symbols::try_from(&bytes)?;

        if self.flash {
            run_flash_download(
                &mut session,
                Path::new(&self.run.path),
                &FlashOptions {
                    disable_progressbars: false,
                    disable_double_buffering: self.run.disable_double_buffering,
                    reset_halt: false,
                    log: None,
                    restore_unwritten: false,
                    flash_layout_output_path: None,
                    elf: None,
                    work_dir: None,
                    cargo_options: CargoOptions::default(),
                    probe_options: self.run.common,
                },
                loader,
                self.run.chip_erase,
            )?;
        }

        let mut core = session.core(self.core)?;
        info!("Attached to Core {}", self.core);
        core.reset()?;

        let start = Instant::now();
        let mut reads = 0;
        let mut samples: HashMap<u32, u64> = HashMap::with_capacity(256 * (self.duration as usize));
        let duration = Duration::from_secs(self.duration);
        let pc_reg = core.program_counter();
        info!("Profiling...");
        loop {
            core.halt(std::time::Duration::from_millis(10))?;
            let pc: u32 = core.read_core_reg(pc_reg)?;
            *samples.entry(pc).or_insert(1) += 1;
            reads += 1;
            core.run()?;
            if Instant::now() - start > duration {
                break;
            }
        }

        let mut v = Vec::from_iter(samples);
        // sort by frequency
        v.sort_by(|&(_, a), &(_, b)| b.cmp(&a));

        for (address, count) in v.into_iter().take(self.limit) {
            let name = symbols
                .get_name(address as u64)
                .unwrap_or(format!("UNKNOWN - {:08X}", address));
            let (file, num) = symbols
                .get_location(address as u64)
                .unwrap_or(("UNKNOWN".to_owned(), 0));
            if self.line_info {
                println!("{}:{}", file, num);
            }
            println!(
                "{:>50} - {:.01}%",
                name,
                (count as f64 / reads as f64) * 100.0
            );
        }

        Ok(())
    }
}

// Wrapper around addr2line that allows to look up function names
pub(crate) struct Symbols<'sym> {
    file: ObjectFile<'sym, &'sym [u8]>,
    ctx: ObjectContext<EndianRcSlice<RunTimeEndian>>,
}

impl<'sym> Symbols<'sym> {
    pub fn try_from(bytes: &'sym [u8]) -> anyhow::Result<Self> {
        let file = ObjectFile::parse(bytes)?;
        let ctx = ObjectContext::new(&file)?;

        Ok(Self { file, ctx })
    }

    /// Returns the name of the function at the given address, if one can be found.
    pub fn get_name(&self, addr: u64) -> Option<String> {
        // The basic steps here are:
        //   1. find which frame `addr` is in
        //   2. look up and demangle the function name
        //   3. if no function name is found, try to look it up in the object file
        //      directly
        //   4. return a demangled function name, if one was found
        let mut frames = match self.ctx.find_frames(addr) {
            LookupResult::Output(result) => result.unwrap(),
            LookupResult::Load { .. } => unimplemented!(),
        };

        frames
            .next()
            .ok()
            .flatten()
            .and_then(|frame| {
                frame
                    .function
                    .and_then(|name| name.demangle().map(|s| s.into_owned()).ok())
            })
            .or_else(|| {
                self.file
                    .symbol_map()
                    .get(addr)
                    .map(|sym| sym.name().to_string())
            })
    }

    /// Returns the file name and line number of the function at the given address, if one can be.
    pub fn get_location(&self, addr: u64) -> Option<(String, u32)> {
        // Find the location which `addr` is in. If we can dedetermine a file name and
        // line number for this function we will return them both in a tuple.
        self.ctx.find_location(addr).ok()?.map(|location| {
            let file = location.file.map(|f| f.to_string());
            let line = location.line;

            match (file, line) {
                (Some(file), Some(line)) => Some((file, line)),
                _ => None,
            }
        })?
    }
}
