use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use itm::TracePacket;
use probe_rs::{
    architecture::arm::{
        component::{find_component, Dwt, TraceSink},
        memory::PeripheralType,
        DpAddress, SwoConfig,
    },
    probe::list::Lister,
};

use addr2line::{
    gimli::{EndianRcSlice, RunTimeEndian},
    object::{read::File as ObjectFile, Object},
    Context as ObjectContext, LookupResult,
};

use crate::util::flash::{build_loader, run_flash_download};
use tracing::info;

#[derive(clap::Parser)]
pub struct ProfileCmd {
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
    /// Profile Method
    #[clap(subcommand)]
    method: ProfileMethod,
}

#[derive(clap::Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileMethod {
    /// Naive, Halt -> Read PC -> Resume profiler
    #[clap(name = "naive")]
    Naive,
    /// Use the Itm port to profile the chip (ARM only)
    #[clap(name = "itm")]
    Itm {
        /// The speed of the clock feeding the TPIU/SWO module in Hz.
        clk: u32,
        /// The desired baud rate of the SWO output.
        baud: u32,
    },
}

impl core::fmt::Display for ProfileMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let s = format!("{:?}", self);
        write!(f, "{}", s.to_lowercase())
    }
}

impl ProfileCmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, probe_options) = self
            .run
            .shared_options
            .probe_options
            .simple_attach(lister)?;

        let loader = build_loader(
            &mut session,
            &self.run.shared_options.path,
            self.run.shared_options.format_options,
            None,
        )?;

        let bytes = std::fs::read(&self.run.shared_options.path)?;
        let symbols = Symbols::try_from(&bytes)?;

        if self.flash {
            run_flash_download(
                &mut session,
                Path::new(&self.run.shared_options.path),
                &self.run.shared_options.download_options,
                &probe_options,
                loader,
                self.run.shared_options.chip_erase,
            )?;
        }

        let start = Instant::now();
        let mut reads = 0;
        let mut samples: HashMap<u32, u64> = HashMap::with_capacity(256 * (self.duration as usize));
        let duration = Duration::from_secs(self.duration);
        info!("Profiling...");

        match self.method {
            ProfileMethod::Naive => {
                let mut core = session.core(self.core)?;
                info!("Attached to Core {}", self.core);
                core.reset()?;
                let pc_reg = core.program_counter();

                loop {
                    core.halt(std::time::Duration::from_millis(10))?;
                    let pc: u32 = core.read_core_reg(pc_reg)?;
                    *samples.entry(pc).or_insert(1) += 1;
                    reads += 1;
                    core.run()?;
                    if start.elapsed() > duration {
                        break;
                    }
                }
            }
            ProfileMethod::Itm { clk, baud } => {
                let sink = TraceSink::Swo(SwoConfig::new(clk).set_baud(baud));
                session.setup_tracing(self.core, sink)?;

                let components = session.get_arm_components(DpAddress::Default)?;
                let component = find_component(&components, PeripheralType::Dwt)?;
                let interface = session.get_arm_interface(0)?;
                let mut dwt = Dwt::new(interface, component);
                dwt.enable_pc_sampling()?;

                let decoder = itm::Decoder::new(
                    session.swo_reader()?,
                    itm::DecoderOptions { ignore_eof: true },
                );

                let iter = decoder.singles();

                for packet in iter {
                    if let TracePacket::PCSample { pc: Some(pc) } = packet? {
                        *samples.entry(pc).or_insert(1) += 1;
                        reads += 1;
                    }
                    if start.elapsed() > duration {
                        break;
                    }
                }
            }
        }

        let mut v = Vec::from_iter(samples);
        // sort by frequency
        v.sort_by(|&(_, a), &(_, b)| b.cmp(&a));

        println!("Samples {}", reads);

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
