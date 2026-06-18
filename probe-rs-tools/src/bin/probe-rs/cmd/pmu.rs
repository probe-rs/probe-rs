//! `probe-rs pmu`: measure CPU performance counters on an ARM Cortex-A9.
//!
//! Halts the target, configures the PMU, resumes, waits for the requested
//! duration, re-halts, then prints cycle count and event counter values.
//!
//! Typical usage:
//! ```
//! # Count cycle + branch mispredictions for 2 seconds:
//! probe-rs pmu --chip <chip> --duration-ms 2000 --events branch-mispredict,l1d-cache-refill
//!
//! # Cycle count only (no --events needed):
//! probe-rs pmu --chip <chip> --duration-ms 1000
//! ```

use std::time::Duration;

use probe_rs::architecture::arm::component::PmuEvent;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;

use crate::CoreOptions;
use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// How long to run the target while counting events (milliseconds).
    #[clap(long, default_value = "1000")]
    duration_ms: u64,

    /// Comma-separated list of PMU events to count.
    ///
    /// Supported names (Cortex-A9 DDI0388 Table 11-23):
    ///   sw-incr, l1i-cache-refill, itlb-refill, l1d-cache-refill, l1d-cache, dtlb-refill,
    ///   ld-retired, st-retired, inst-retired, exc-taken, exc-return, cid-write-retired,
    ///   pc-write-retired, br-immed-retired, br-return-retired, unaligned-ldst-retired,
    ///   br-mis-pred, cpu-cycles, br-pred,
    ///   coherent-linefill-miss, coherent-linefill-hit, icache-stall, dcache-stall,
    ///   main-tlb-stall, strex-passed, strex-failed, data-eviction, data-linefill
    ///
    /// Up to 6 events can be measured simultaneously (hardware limit).
    #[clap(long, value_delimiter = ',')]
    events: Vec<String>,
}

impl Cmd {
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(registry, lister)?;

        // Parse event names.
        let events: Vec<PmuEvent> = self
            .events
            .iter()
            .map(|s| parse_event(s))
            .collect::<Result<_, _>>()?;

        let duration = Duration::from_millis(self.duration_ms);

        eprintln!(
            "Profiling core {} for {} ms…",
            self.shared.core, self.duration_ms
        );

        let snapshot = session.pmu_profile(self.shared.core, &events, duration)?;

        // Print results.
        println!("cycles: {}", snapshot.cycles);
        for (event, count) in &snapshot.events {
            println!("{event}: {count}");
        }

        Ok(())
    }
}

/// Map a kebab-case event name to a [`PmuEvent`] variant.
fn parse_event(name: &str) -> anyhow::Result<PmuEvent> {
    match name.to_ascii_lowercase().as_str() {
        "sw-incr" | "software-increment" => Ok(PmuEvent::SoftwareIncrement),
        "l1i-cache-refill" => Ok(PmuEvent::L1ICacheRefill),
        "itlb-refill" => Ok(PmuEvent::ItlbRefill),
        "l1d-cache-refill" => Ok(PmuEvent::L1DCacheRefill),
        "l1d-cache" => Ok(PmuEvent::L1DCacheAccess),
        "dtlb-refill" => Ok(PmuEvent::DtlbRefill),
        "ld-retired" => Ok(PmuEvent::DataRead),
        "st-retired" => Ok(PmuEvent::DataWrite),
        "inst-retired" => Ok(PmuEvent::InstructionExecuted),
        "exc-taken" => Ok(PmuEvent::ExceptionTaken),
        "exc-return" => Ok(PmuEvent::ExceptionReturn),
        "cid-write-retired" => Ok(PmuEvent::ContextIdRetired),
        "pc-write-retired" | "sw-change-pc" => Ok(PmuEvent::SWChangePC),
        "br-immed-retired" => Ok(PmuEvent::ImmBranchExecuted),
        "br-return-retired" | "procedure-call" => Ok(PmuEvent::ProcedureCall),
        "unaligned-ldst-retired" => Ok(PmuEvent::UnalignedAccess),
        "br-mis-pred" | "branch-mispredict" => Ok(PmuEvent::BranchMispredict),
        "cpu-cycles" => Ok(PmuEvent::CycleCountAlias),
        "br-pred" | "branch-predicted" => Ok(PmuEvent::BranchPredicted),
        // Cortex-A9-specific events (DDI0388-i Table 11.6).
        "coherent-linefill-miss" => Ok(PmuEvent::CoherentLinefillMiss),
        "coherent-linefill-hit" => Ok(PmuEvent::CoherentLinefillHit),
        "icache-stall" => Ok(PmuEvent::ICacheStall),
        "dcache-stall" => Ok(PmuEvent::DCacheStall),
        "main-tlb-stall" => Ok(PmuEvent::MainTlbStall),
        "strex-passed" => Ok(PmuEvent::StrexPassed),
        "strex-failed" => Ok(PmuEvent::StrexFailed),
        "data-eviction" => Ok(PmuEvent::DataEviction),
        "data-linefill" => Ok(PmuEvent::DataLinefill),
        other => anyhow::bail!(
            "Unknown PMU event '{other}'. Run `probe-rs pmu --help` for the list of supported events."
        ),
    }
}
