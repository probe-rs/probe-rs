use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use addr2line::Loader;
use anyhow::anyhow;
use itm::TracePacket;
use probe_rs::config::Registry;
use probe_rs::{
    architecture::arm::{
        SwoConfig,
        component::{Dwt, TraceSink, enable_tracing, find_component},
        dp::DpAddress,
        memory::PeripheralType,
    },
    probe::list::Lister,
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
    /// Use the DWT_PCSR to profile the chip (ARM only)
    #[clap(name = "pcsr")]
    Pcsr,
}

impl std::fmt::Display for ProfileMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let s = format!("{:?}", self);
        write!(f, "{}", s.to_lowercase())
    }
}

impl ProfileCmd {
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, probe_options) = self
            .run
            .shared_options
            .probe_options
            .simple_attach(registry, lister)?;

        let loader = build_loader(
            &mut session,
            &self.run.shared_options.path,
            self.run.shared_options.format_options,
            None,
        )?;

        let file_location = self.run.shared_options.path.as_path();

        // The error returned from try_from cannot be converted directly to anyhow::Error unfortunately,
        // due to a limitation in addr2line.
        let symbols = Symbols::try_from(file_location).map_err(|e| {
            anyhow!(
                "Failed to read symbol data from {}: {}",
                file_location.display(),
                e
            )
        })?;

        if self.flash {
            run_flash_download(
                &mut session,
                file_location,
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
                    core.halt(Duration::from_millis(10))?;
                    let pc: u32 = core.read_core_reg(pc_reg)?;
                    *samples.entry(pc).or_insert(1) += 1;
                    reads += 1;
                    core.run()?;
                    if start.elapsed() > duration {
                        break;
                    }
                }
            }
            ProfileMethod::Pcsr => {
                enable_tracing(&mut session.core(self.core)?)?;

                let components = session.get_arm_components(DpAddress::Default)?;
                let component = find_component(&components, PeripheralType::Dwt)?;
                let interface = session.get_arm_interface()?;

                let mut dwt = Dwt::new(interface, component);
                dwt.enable()?;

                while start.elapsed() <= duration {
                    let pc = dwt.read_pcsr()?;
                    *samples.entry(pc).or_insert(1) += 1;
                    reads += 1;
                }
            }
            ProfileMethod::Itm { clk, baud } => {
                let sink = TraceSink::Swo(SwoConfig::new(clk).set_baud(baud));
                session.setup_tracing(self.core, sink)?;

                let components = session.get_arm_components(DpAddress::Default)?;
                let component = find_component(&components, PeripheralType::Dwt)?;
                let interface = session.get_arm_interface()?;
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
            if self.line_info {
                let (file, num) = symbols
                    .get_location(address as u64)
                    .unwrap_or(("UNKNOWN", 0));
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
pub(crate) struct Symbols {
    loader: Loader,
}

impl Symbols {
    pub fn try_from(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let loader = Loader::new(path)?;
        Ok(Self { loader })
    }

    /// Returns the name of the function at the given address, if one can be found.
    pub fn get_name(&self, addr: u64) -> Option<String> {
        // The basic steps here are:
        //   1. find which frame `addr` is in
        //   2. look up and demangle the function name
        //   3. if no function name is found, try to look it up in the object file
        //      directly
        //   4. return a demangled function name, if one was found
        let mut frames = self.loader.find_frames(addr).ok()?;

        frames
            .next()
            .ok()
            .flatten()
            .and_then(|frame| {
                frame
                    .function
                    .and_then(|name| name.demangle().map(|s| s.into_owned()).ok())
            })
            .or_else(|| self.loader.find_symbol(addr).map(|sym| sym.to_string()))
    }

    /// Returns the file name and line number of the function at the given address, if one can be.
    pub fn get_location(&self, addr: u64) -> Option<(&str, u32)> {
        // Find the location which `addr` is in. If we can determine a file name and
        // line number for this function we will return them both in a tuple.
        self.loader.find_location(addr).ok()?.and_then(|location| {
            let file = location.file?;
            let line = location.line?;

            Some((file, line))
        })
    }
}
