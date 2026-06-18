//! Performance Monitoring Unit (PMU) driver for ARM Cortex-A9.
//!
//! The PMU is a CoreSight component accessed via the APB debug bus.  Its base
//! address is discovered automatically through the ROM-table walk
//! (`PeripheralType::Pmu`).  All register accesses go through the standard
//! `component.read_reg` / `component.write_reg` helpers so the core does not
//! need to be halted during readout.
//!
//! **Register offsets** follow the IHI0029 "ARM Performance Monitors
//! Architecture" external debug register map (PMUv2).  Cross-reference:
//! ARM DDI 0388I (Cortex-A9 TRM r4p1) §11.3 "PMU External register summary".

use crate::architecture::arm::{ArmDebugInterface, ArmError, memory::CoresightComponent};

// External debug register offsets (from PMU component base address)
// Cortex-A9 PMU memory-mapped (APB, PADDRDBG[12]=1) register offsets, DDI0388-i Table 11.1.
const PMEVCNTR_BASE: u32 = 0x000; // PMXEVCNTRn = base + 4*n   (n = 0..5)
const PMCCNTR: u32 = 0x07C; // Cycle counter, register #31 (32-bit)
const PMEVTYPER_BASE: u32 = 0x400; // PMXEVTYPERn = base + 4*n  (n = 0..5)
const PMCNTENSET: u32 = 0xC00; // Counter enable set
const PMCNTENCLR: u32 = 0xC20; // Counter enable clear
const PMOVSR: u32 = 0xC80; // Overflow flag status (write 1 to clear)
const PMCR: u32 = 0xE04; // PMU control register

// PMCR bit fields 
const PMCR_E: u32 = 1 << 0; // Global enable
const PMCR_P: u32 = 1 << 1; // Reset all event counters to 0
const PMCR_C: u32 = 1 << 2; // Reset cycle counter to 0
#[allow(dead_code)]
const PMCR_D: u32 = 1 << 3; // Clock divider (0 = count every cycle)
const PMCR_N_SHIFT: u32 = 11; // N-field: number of event counters [15:11]
const PMCR_N_MASK: u32 = 0x1F;

// PMCNTENSET / PMCNTENCLR bit 31 = cycle counter
const PMCNTEN_CCNTR: u32 = 1 << 31;

/// Cortex-A9 PMU hardware event selectors.
///
/// Values are the 8-bit event identifiers written into PMEVTYPERn
/// (ARM DDI 0388I Table 11-23).
/// Only events the Cortex-A9 actually implements are exposed.
///
/// The A9 implements architectural events `0x00`–`0x12` (DDI0388-i Table 11.5) — but with two
/// gaps: `0x08` (INST_RETIRED) and `0x0E` (BR_RETURN_RETIRED) are **not** implemented; the A9
/// provides equivalents at `0x68` and `0x6E`. The generic ARMv7 "common" events `0x13`–`0x1E`
/// are not implemented on the A9 (it would silently count nothing); the useful microarchitectural
/// counters live in the A9-specific `0x40`–`0x6B` range instead (DDI0388-i Table 11.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PmuEvent {
    /// Software-triggered increment via PMSWINC.
    SoftwareIncrement = 0x00,
    /// Instruction fetch that causes a refill in the L1 instruction cache.
    L1ICacheRefill = 0x01,
    /// Instruction TLB refill.
    ItlbRefill = 0x02,
    /// Data access that causes a refill in the L1 data cache.
    L1DCacheRefill = 0x03,
    /// Data or unified cache access.
    L1DCacheAccess = 0x04,
    /// Data TLB refill.
    DtlbRefill = 0x05,
    /// Data reads (including SWP, LDM, etc.).
    DataRead = 0x06,
    /// Data writes (including SWP, STM, etc.).
    DataWrite = 0x07,
    /// Exception taken.
    ExceptionTaken = 0x09,
    /// Exception return executed.
    ExceptionReturn = 0x0A,
    /// Change to ContextID retired.
    ContextIdRetired = 0x0B,
    /// Software change of PC.
    SWChangePC = 0x0C,
    /// Immediate branch that is architecturally executed.
    ImmBranchExecuted = 0x0D,
    /// Unaligned load or store executed.
    UnalignedAccess = 0x0F,
    /// Branch mispredicted or not predicted.
    BranchMispredict = 0x10,
    /// Cycle counter (alias — normally the PMCCNTR is used directly).
    CycleCountAlias = 0x11,
    /// Predictable branches speculatively executed.
    BranchPredicted = 0x12,

    // -- Cortex-A9-specific events (DDI0388-i Table 11.6) ------------------------
    /// Approximate instructions executed: count of instructions leaving the register
    /// rename stage (A9 has no architectural `0x08`/INST_RETIRED counter).
    InstructionExecuted = 0x68,
    /// Predictable function returns (A9 equivalent of `0x0E`/BR_RETURN_RETIRED).
    ProcedureCall = 0x6E,
    /// Coherent linefill that missed in all other cores (fetched from external memory).
    CoherentLinefillMiss = 0x50,
    /// Coherent linefill that hit in another core's cache.
    CoherentLinefillHit = 0x51,
    /// Instruction-cache dependent stall cycles.
    ICacheStall = 0x60,
    /// Data-cache dependent stall cycles.
    DCacheStall = 0x61,
    /// Main TLB miss stall cycles.
    MainTlbStall = 0x62,
    /// STREX instructions that passed.
    StrexPassed = 0x63,
    /// STREX instructions that failed.
    StrexFailed = 0x64,
    /// Data evictions caused by a linefill (closest A9 analogue of an L1D write-back).
    DataEviction = 0x65,
    /// Data linefills performed on the external AXI bus.
    DataLinefill = 0x69,
}

impl std::fmt::Display for PmuEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::SoftwareIncrement => "SW_INCR",
            Self::L1ICacheRefill => "L1I_CACHE_REFILL",
            Self::ItlbRefill => "L1I_TLB_REFILL",
            Self::L1DCacheRefill => "L1D_CACHE_REFILL",
            Self::L1DCacheAccess => "L1D_CACHE",
            Self::DtlbRefill => "L1D_TLB_REFILL",
            Self::DataRead => "LD_RETIRED",
            Self::DataWrite => "ST_RETIRED",
            Self::ExceptionTaken => "EXC_TAKEN",
            Self::ExceptionReturn => "EXC_RETURN",
            Self::ContextIdRetired => "CID_WRITE_RETIRED",
            Self::SWChangePC => "PC_WRITE_RETIRED",
            Self::ImmBranchExecuted => "BR_IMMED_RETIRED",
            Self::UnalignedAccess => "UNALIGNED_LDST_RETIRED",
            Self::BranchMispredict => "BR_MIS_PRED",
            Self::CycleCountAlias => "CPU_CYCLES",
            Self::BranchPredicted => "BR_PRED",
            Self::InstructionExecuted => "INST_RETIRED_APPROX",
            Self::ProcedureCall => "BR_RETURN_RETIRED",
            Self::CoherentLinefillMiss => "COHERENT_LINEFILL_MISS",
            Self::CoherentLinefillHit => "COHERENT_LINEFILL_HIT",
            Self::ICacheStall => "ICACHE_STALL",
            Self::DCacheStall => "DCACHE_STALL",
            Self::MainTlbStall => "MAIN_TLB_STALL",
            Self::StrexPassed => "STREX_PASSED",
            Self::StrexFailed => "STREX_FAILED",
            Self::DataEviction => "DATA_EVICTION",
            Self::DataLinefill => "DATA_LINEFILL",
        };
        write!(f, "{s}")
    }
}

/// A snapshot of PMU counter values taken at one instant.
#[derive(Debug, Clone)]
pub struct PmuSnapshot {
    /// Cycle counter value (PMCCNTR).
    pub cycles: u32,
    /// (Event selector, count) pairs for each configured event counter slot.
    pub events: Vec<(PmuEvent, u32)>,
}

/// Driver for the Cortex-A9 Performance Monitoring Unit.
pub struct PerformanceMonitoringUnit<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmDebugInterface,
}

impl<'a> PerformanceMonitoringUnit<'a> {
    /// Attach to the PMU CoreSight component.
    pub fn new(
        interface: &'a mut dyn ArmDebugInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Self {
            component,
            interface,
        }
    }

    /// Read the PMCR.N field: number of event counters implemented.
    pub fn n_counters(&mut self) -> Result<u8, ArmError> {
        let pmcr = self.component.read_reg(self.interface, PMCR)?;
        Ok(((pmcr >> PMCR_N_SHIFT) & PMCR_N_MASK) as u8)
    }

    /// Reset and configure the PMU to count the given events.
    ///
    /// - Disables all counters.
    /// - Resets cycle counter and all event counters to zero.
    /// - Programs each available event counter slot with the requested event.
    /// - Enables the cycle counter and requested event counters.
    /// - Enables the PMU globally.
    ///
    /// Slots beyond the hardware N-counter limit are silently ignored.
    pub fn configure(&mut self, events: &[PmuEvent]) -> Result<(), ArmError> {
        // 1. Disable all counters while we configure them.
        self.component
            .write_reg(self.interface, PMCNTENCLR, 0xFFFF_FFFF)?;

        // 2. Reset cycle counter + event counters, disable clock divider.
        self.component.write_reg(
            self.interface,
            PMCR,
            PMCR_P | PMCR_C, // reset both, keep E=0 for now
        )?;

        // 3. Clear overflow flags.
        self.component
            .write_reg(self.interface, PMOVSR, 0xFFFF_FFFF)?;

        // 4. Program event types.
        let n = self.n_counters()? as usize;
        let n_to_configure = events.len().min(n);

        for (i, event) in events.iter().enumerate().take(n_to_configure) {
            self.component.write_reg(
                self.interface,
                PMEVTYPER_BASE + 4 * i as u32,
                *event as u32,
            )?;
        }

        // 5. Build enable mask: bit 31 = CCNTR, bits 0..n_to_configure-1 = event counters.
        let enable_mask = PMCNTEN_CCNTR | ((1u32 << n_to_configure) - 1);
        self.component
            .write_reg(self.interface, PMCNTENSET, enable_mask)?;

        // 6. Enable the PMU globally (PMCR.E = 1).
        self.component.write_reg(self.interface, PMCR, PMCR_E)?;

        Ok(())
    }

    /// Read a snapshot of the current counter values.
    ///
    /// `events` must match the slice passed to `configure` so the result can be
    /// labelled correctly.
    pub fn read_results(&mut self, events: &[PmuEvent]) -> Result<PmuSnapshot, ArmError> {
        let cycles = self.component.read_reg(self.interface, PMCCNTR)?;

        let n = self.n_counters()? as usize;
        let n_to_read = events.len().min(n);

        let mut event_counts = Vec::with_capacity(n_to_read);
        for (i, &event) in events.iter().enumerate().take(n_to_read) {
            let count = self
                .component
                .read_reg(self.interface, PMEVCNTR_BASE + 4 * i as u32)?;
            event_counts.push((event, count));
        }

        Ok(PmuSnapshot {
            cycles,
            events: event_counts,
        })
    }

    /// Disable the PMU (PMCR.E = 0, all counters disabled).
    pub fn disable(&mut self) -> Result<(), ArmError> {
        self.component
            .write_reg(self.interface, PMCNTENCLR, 0xFFFF_FFFF)?;
        self.component.write_reg(self.interface, PMCR, 0)?;
        Ok(())
    }
}
