//! Defining the interface for ETMv4.
//!
//! ETMv4 is the latest version of the embedded trace macrocell.
//! For more information see Arm Embedded Trace Macrocell Architecture Specification
//! ETMv4.0 to ETMv4.6

use std::time::Duration;

use crate::architecture::arm::component::DebugComponentInterface;
use crate::architecture::arm::memory::romtable::CoresightComponent;
use crate::architecture::arm::{ArmDebugInterface, ArmError};
use crate::memory_mapped_bitfield_register;

use super::EtmPacket;

// [`EtmV4`] Configuration that can be supplied to a config function.
pub struct EtmV4Config {
    /// Output cycle information for each basic block.
    /// Greatly increases the trace size.
    pub cyc_acc: bool,
    /// Sets the cycle count packet output threshold.
    pub cc_threshold: u32,
    /// If the return stack feature is enabled.
    pub ret_stack: bool,
    /// Trace ID of the packets. Used by the trace analyzer and to distinguish
    /// the packets between other trace sources like ITM.
    pub trace_id: u32,
    /// Defines what DWT unit should be used as start signal for the ETM.
    pub start_unit: u32,
    /// Defines what DWT unit should be used as stop signal for the ETM.
    pub stop_unit: u32,
}

impl Default for EtmV4Config {
    fn default() -> Self {
        Self {
            cyc_acc: true,
            cc_threshold: 1,
            ret_stack: false,
            trace_id: 0x3E,
            start_unit: 0b0001,
            stop_unit: 0b00010,
        }
    }
}

/// Struct represting ETM and its associated configurations.
pub struct EtmV4<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmDebugInterface,
}

impl<'a> EtmV4<'a> {
    /// Construct a new ETM instance.
    pub fn new(
        interface: &'a mut dyn ArmDebugInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        EtmV4 {
            interface,
            component,
        }
    }
    /// Log information about the [`EtmV4`] module.
    pub fn info(&mut self) -> Result<(), ArmError> {
        let stat = Stat::load(self.component, self.interface)?;
        tracing::info!("ETM info:");
        tracing::info!("  Is idle: {}", stat.idle());
        tracing::info!("  Stable programming state: {}", stat.pmstable());
        Ok(())
    }

    /// Configure the [`EtmV4`] for instruction trace.
    pub fn enable_instruction_trace(&mut self, etm_config: &EtmV4Config) -> Result<(), ArmError> {
        // Stop the ETM. This should be done before any configuration because in
        // most SoCs the power domains are not the same and configuration is
        // preserved between reboots.
        self.stop()?;
        // Check whether the module is inactive or not.
        self.idle()?;
        // Load the Config register which controls the programming mode.
        let mut config = Config::load(self.component, self.interface)?;
        config.set_bb(true);
        config.set_cci(etm_config.cyc_acc);
        // Conditional tracing is not supported yet.
        config.set_cond(0b00);
        config.set_ts(false);
        config.set_rs(false);
        config.store(self.component, self.interface)?;

        // Set the stalling to true and into its highest settings. Losing
        // trace data means that something went wrong.
        let mut stall = StallCtl::load(self.component, self.interface)?;
        stall.set_istall(true);
        stall.set_dstall(true);
        stall.set_level(0b11);
        stall.store(self.component, self.interface)?;

        // Load the CCCTL register which controls the cycle count threshold.
        if etm_config.cyc_acc {
            let mut cnt = CcCtl::load(self.component, self.interface)?;
            cnt.set_threshold(etm_config.cc_threshold);
            cnt.store(self.component, self.interface)?;
        }

        // Load the TRACEID register which controls the traceid of the output.
        let mut id = TraceId::load(self.component, self.interface)?;
        id.set_traceid(etm_config.trace_id);
        id.store(self.component, self.interface)?;

        // Set the viewinst signal to the stopped state.
        let mut victl = ViCtl::load(self.component, self.interface)?;
        victl.set_rtype(false);
        victl.set_sel(0b0001);
        victl.set_ssstatus(false);
        victl.store(self.component, self.interface)?;

        // Choose the comparator for the start viewinst signal.
        let mut vipcssctl = VipcssCtl::load(self.component, self.interface)?;
        vipcssctl.set_start(etm_config.start_unit);
        vipcssctl.set_stop(etm_config.stop_unit);
        vipcssctl.store(self.component, self.interface)?;

        // Reload the register and Start the Etm.
        let mut ctrl = PrgCtrl::load(self.component, self.interface)?;
        ctrl.set_en(true);
        ctrl.store(self.component, self.interface)?;

        Ok(())
    }

    /// Stops the [`EtmV4`]
    pub fn stop(&mut self) -> Result<(), ArmError> {
        let mut ctrl = PrgCtrl::load(self.component, self.interface)?;
        ctrl.set_en(false);
        ctrl.store(self.component, self.interface)?;

        // Set the viewinst signal to the stopped state. Required if the module
        // has not reached an stopped state.
        let mut victl = ViCtl::load(self.component, self.interface)?;
        victl.set_ssstatus(false);
        victl.store(self.component, self.interface)?;

        Ok(())
    }

    /// Checks whether the [`EtmV4`] is idle or not.
    pub fn idle(&mut self) -> Result<bool, ArmError> {
        for attempt in 0..=12 {
            let stat = Stat::load(self.component, self.interface)?;
            if stat.idle() {
                return Ok(true);
            }
            tracing::info!("Module is not IDLE at attempt {attempt}, {stat:?}");
            std::thread::sleep(Duration::from_micros(50 << attempt));
        }
        Ok(false)
    }

    /// Returns the decoder.
    pub fn decoder(&mut self) -> Result<EtmV4Decoder, ArmError> {
        let cnt = CcCtl::load(self.component, self.interface)?;
        let idr8 = IDR8::load(self.component, self.interface)?;
        Ok(EtmV4Decoder::new(idr8.maxspec(), cnt.threshold()))
    }
}

// Programmer Control . This helps us enable and start the module.
memory_mapped_bitfield_register! {
    pub struct PrgCtrl(u32);
    0x004, "ETM/PRGCTRL",
    impl From;
    pub en, set_en: 0;
}

impl DebugComponentInterface for PrgCtrl {}

// Etm status register.
memory_mapped_bitfield_register! {
    pub struct Stat(u32);
    0x00c, "ETM/STAT",
    impl From;
    pub idle, _: 0;
    pub pmstable, _: 1;
}

impl DebugComponentInterface for Stat {}

// Etm configuration register.
memory_mapped_bitfield_register! {
    pub struct Config(u32);
    0x010, "ETM/CONFIG",
    impl From;
    // Branch broadcasting mode
    pub bb, set_bb: 3;
    // Cycle counting in instruction trace
    pub cci, set_cci: 4;
    // conditional instruction tracing
    pub cond, set_cond: 10, 8;
    // Global timestamping
    pub ts, set_ts: 11;
    // Return stack enable
    pub rs, set_rs: 12;
}

impl DebugComponentInterface for Config {}

memory_mapped_bitfield_register! {
    pub struct EventCtl0(u32);
    0x020, "ETM/EVENTCTL0",
    impl From;
    // Depends on type0
    pub sel0, set_sel0: 3, 0;
    // resource type for event 0
    pub type0, set_type0: 7;
    pub sel1, set_sel1: 11, 8;
    // resource type for event 1
    pub type1, set_type1: 15;
}

impl DebugComponentInterface for EventCtl0 {}

memory_mapped_bitfield_register! {
    pub struct EventCtl1(u32);
    0x024, "ETM/EVENTCTL1",
    impl From;
    pub insten, set_insten: 3, 0;
    pub atb, set_atb: 11;
    pub lpoverride, set_lpoverride: 12;
}

impl DebugComponentInterface for EventCtl1 {}

memory_mapped_bitfield_register! {
    pub struct StallCtl(u32);
    0x02c, "ETM/STALLCTL",
    impl From;
    // Higher the level the more stall we get
    pub level, set_level: 3, 2;
    // Instruction stall
    pub istall, set_istall: 8;
    // Data stall
    pub dstall, set_dstall: 9;
}

impl DebugComponentInterface for StallCtl {}

memory_mapped_bitfield_register! {
    pub struct Syncp(u32);
    0x034, "ETM/SYNCP",
    impl From;
    pub period, _: 4,0;
}

impl DebugComponentInterface for Syncp {}

memory_mapped_bitfield_register! {
    pub struct CcCtl(u32);
    0x038, "ETM/CCCTL",
    impl From;
    pub threshold, set_threshold: 11, 0;
}

impl DebugComponentInterface for CcCtl {}

memory_mapped_bitfield_register! {
    pub struct TraceId(u32);
    0x040, "ETM/TRACEID",
    impl From;
    pub traceid, set_traceid: 6, 0;
}

impl DebugComponentInterface for TraceId {}

memory_mapped_bitfield_register! {
    pub struct ViCtl(u32);
    0x080, "ETM/VICTL",
    impl From;
    pub sel, set_sel: 3, 0;
    // Changed from type to avoid collision between keywords
    pub rtype, set_rtype: 7;
    pub ssstatus, set_ssstatus: 9;
    pub trcrest, set_trcrest: 10;
    pub trcerr, set_trcerr: 11;
    pub exlevel_s0, set_exlevel_s0: 16;
    pub exlevel_s3, set_exlevel_s3: 19;
}

impl DebugComponentInterface for ViCtl {}

memory_mapped_bitfield_register! {
    pub struct VissCtl(u32);
    0x088, "ETM/VISSCTL",
    impl From;
    pub start, set_start: 7, 0;
    pub stop, set_stop: 23, 16;
}

impl DebugComponentInterface for VissCtl {}

memory_mapped_bitfield_register! {
    pub struct VipcssCtl(u32);
    0x08c, "ETM/VIPCSSCTL",
    impl From;
    pub start, set_start: 3, 0;
    pub stop, set_stop: 19, 16;
}

impl DebugComponentInterface for VipcssCtl {}

memory_mapped_bitfield_register! {
    pub struct Cntrldv(u32);
    0x140, "ETM/CNTRLDV",
    impl From;
    pub value, set_value: 15, 0;
}

impl DebugComponentInterface for Cntrldv {}

// ID register were not mapped for now :^)

// Resource control registers

memory_mapped_bitfield_register! {
    pub struct RsCtl2(u32);
    0x208, "ETM/RSCTL2",
    impl From;
    pub select, set_select: 7, 0;
    pub group, set_group: 18, 16;
    pub inv, set_inv: 20;
    pub pairinv, set_pairinv: 21;
}

impl DebugComponentInterface for RsCtl2 {}

memory_mapped_bitfield_register! {
    pub struct RsCtl3(u32);
    0x20c, "ETM/RSCTL3",
    impl From;
    pub select, set_select: 7, 0;
    pub group, set_group: 18, 16;
    pub inv, set_inv: 20;
    pub pairinv, set_pairinv: 21;
}

impl DebugComponentInterface for RsCtl3 {}

memory_mapped_bitfield_register! {
    pub struct Sscc0(u32);
    0x280, "ETM/SSCC0",
    impl From;
    pub rst, set_rst: 24;
}

impl DebugComponentInterface for Sscc0 {}

memory_mapped_bitfield_register! {
    pub struct Sspcic0(u32);
    0x2c0, "ETM/SSPCIC0",
    impl From;
    pub pc, set_pc: 7, 0;
}

impl DebugComponentInterface for Sspcic0 {}

memory_mapped_bitfield_register! {
    pub struct Pdc(u32);
    0x310, "ETM/PDC",
    impl From;
    pub pu, _: 3;
}

impl DebugComponentInterface for Pdc {}

memory_mapped_bitfield_register! {
    pub struct Pds(u32);
    0x314, "ETM/PDS",
    impl From;
    pub power, _: 0;
    pub stickypd, _: 1;
}

impl DebugComponentInterface for Pds {}

memory_mapped_bitfield_register! {
    pub struct ClaimSet(u32);
    0xfa0, "ETM/CLAIMSET",
    impl From;
    pub claimset, set_claimset: 3, 0;
}

impl DebugComponentInterface for ClaimSet {}

memory_mapped_bitfield_register! {
    pub struct ClaimClr(u32);
    0xfa4, "ETM/CLAIMCLR",
    impl From;
    pub claimclr, set_claimclr: 3, 0;
}

impl DebugComponentInterface for ClaimClr {}

// Processor lock. Should be set to 0xC5ACCE55 to enable access from core.
memory_mapped_bitfield_register! {
    pub struct Lar(u32);
    0xfb0, "ETM/LAR",
    impl From;
    pub access_w, set_access_w: 31, 0;
}

impl DebugComponentInterface for Lar {}

memory_mapped_bitfield_register! {
    pub struct Lsr(u32);
    0xfb4, "ETM/LSR",
    impl From;
    pub lockexist, _: 0;
    pub lockgrant, _: 1;
    pub locktype, _: 2;
}

impl DebugComponentInterface for Lsr {}

memory_mapped_bitfield_register! {
    pub struct AuthStat(u32);
    0xfb8, "ETM/AUTHSTAT",
    impl From;
    pub nsid, _: 1, 0;
    pub nsnid, _: 3, 2;
    pub sid, _: 5, 4;
    pub snid, _: 7, 6;
}

impl DebugComponentInterface for AuthStat {}

memory_mapped_bitfield_register! {
    pub struct IDR8(u32);
    0x180, "ETM/IDR8",
    impl From;
    pub maxspec, _: 31, 0;
}

impl DebugComponentInterface for IDR8 {}

/// Header description for the trace elements.
/// Refer to the section 6 of the manual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrClass {
    // All extenstion packets occupy the same header. Should refer to the
    // payload for detection.
    ExtensionPackets,
    TraceInfo,
    Timestamp,
    TraceOn,
    FunctionReturn,
    Exception,
    ExceptionReturn,
    Resync,
    CycleCountFmt1,
    CycleCountFmt2,
    CycleCountFmt3,
    Commit,
    CancelFmt1,
    Mispredict,
    CancelFmt2,
    CancelFmt3,
    ConditionalInstFmt1,
    ConditionalInstFmt2,
    ConditionalInstFmt3,
    ConditionalFlush,
    ConditionalResFmt1,
    ConditionalResFmt2,
    ConditionalResFmt3,
    ConditionalResFmt4,
    Ignore,
    Event,
    Context,
    AddressWithContext,
    TimestampMarker,
    ExactMatchAddress,
    ShortAddress,
    LongAddress32,
    LongAddress64,
    Q,
    AtomFmt1,
    AtomFmt2,
    AtomFmt3,
    AtomFmt4,
    AtomFmt5,
    AtomFmt6,
    ReservedOrUnknown,
}

impl From<u8> for HdrClass {
    fn from(value: u8) -> Self {
        match value {
            0x00 => HdrClass::ExtensionPackets,
            0x01 => HdrClass::TraceInfo,
            0x02..=0x03 => HdrClass::Timestamp,
            0x04 => HdrClass::TraceOn,
            0x05 => HdrClass::FunctionReturn,
            0x06 => HdrClass::Exception,
            0x07 => HdrClass::ExceptionReturn,
            0x08 => HdrClass::Resync,
            0x0C | 0x0D => HdrClass::CycleCountFmt2,
            0x0E | 0x0F => HdrClass::CycleCountFmt1,
            0x10..=0x1F => HdrClass::CycleCountFmt3,
            0x2D => HdrClass::Commit,
            0x2E | 0x2F => HdrClass::CancelFmt1,
            0x30..=0x33 => HdrClass::Mispredict,
            0x34..=0x37 => HdrClass::CancelFmt2,
            0x38..=0x3F => HdrClass::CancelFmt3,
            0x40..=0x42 => HdrClass::ConditionalInstFmt2,
            0x43 => HdrClass::ConditionalFlush,
            0x44..=0x46 => HdrClass::ConditionalResFmt4,
            0x48..=0x4A | 0x4C..=0x4E => HdrClass::ConditionalResFmt2,
            0x50..=0x5F => HdrClass::ConditionalResFmt3,
            0x68..=0x6B => HdrClass::ConditionalResFmt1,
            0x6C => HdrClass::ConditionalInstFmt1,
            0x6D => HdrClass::ConditionalInstFmt3,
            0x6E | 0x6F => HdrClass::ConditionalResFmt1,
            0x70 => HdrClass::Ignore,
            0x71..=0x7F => HdrClass::Event,
            0x80 | 0x81 => HdrClass::Context,
            0x81..=0x83 | 0x85 | 0x86 => HdrClass::AddressWithContext,
            0x88 => HdrClass::TimestampMarker,
            0x90..=0x92 => HdrClass::ExactMatchAddress,
            0x95..=0x96 => HdrClass::ShortAddress,
            0x9A..=0x9B => HdrClass::LongAddress32,
            0x9D..=0x9E => HdrClass::LongAddress64,
            0xA0..=0xAF => HdrClass::Q,
            0xC0..=0xD4 | 0xE0..=0xF4 => HdrClass::AtomFmt6,
            0xD5..=0xD7 | 0xF5 => HdrClass::AtomFmt5,
            0xD8..=0xDB => HdrClass::AtomFmt2,
            0xDC..=0xDF => HdrClass::AtomFmt4,
            0xF6 | 0xF7 => HdrClass::AtomFmt1,
            0xF8 | 0xFF => HdrClass::AtomFmt3,
            _ => HdrClass::ReservedOrUnknown,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddrWidth {
    W32,
    W64,
}

/// [`TraceInfo`] packet contains information about the setup of the trace.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceInfo {
    pub cc: bool,
    pub cc_threshold: u16,
    pub cond: bool,
    pub p0_load: bool,
    pub p0_store: bool,
    pub curr_spec_depth: u32,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum ExceptionType {
    #[default]
    None,
    DebugHalt,
}

// TODO: Fill out all the exception types.
impl From<u64> for ExceptionType {
    fn from(val: u64) -> Self {
        match val {
            0x18 => Self::DebugHalt,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exception {
    pub e0: bool,
    pub etype: ExceptionType,
    pub e1: bool,
    pub p: bool,
}

impl Exception {
    pub fn new(bytes: &[u8]) -> Self {
        let c = ((bytes[0] >> 7) & 0b1) != 0;
        if c {
            Self {
                e0: (bytes[0] & 0b1) != 0,
                etype: (((bytes[0] >> 1) & 0x1f) as u64).into(),
                e1: ((bytes[0] >> 6) & 0b1) != 0,
                p: false,
            }
        } else {
            Self {
                e0: (bytes[0] & 0b1) != 0,
                etype: ((((bytes[1] & 0x1f) << 5) | ((bytes[0] >> 1) & 0x1f)) as u64).into(),
                e1: ((bytes[0] >> 6) & 0b1) != 0,
                p: ((bytes[1] >> 5) & 0b1) != 0,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EtmV4Packet {
    Async,
    Discard,
    Overflow,
    BranchFutureFlush,
    ConditionalFlush,
    Mispredict,
    TraceInfo(TraceInfo),
    TraceOn,
    Addr32(u64),
    Addr64(u64),
    Atom { outcomes: Vec<bool> },
    Exception(Exception),
    ExceptionReturn,
    FunctionReturn,
    Resync,
    CycleCount(u32),
    Commit(u32),
    Timestamp(u64),
    Cancel(u32),
}

enum ParseResult {
    Item { len: usize, pkt: EtmV4Packet },
    Skip { len: usize },
    Multiple { len: usize, pkts: Vec<EtmV4Packet> },
    NeedMore,
}

/// Stores the required state as specified by the trace analyzer protocol in
/// the manual. Not all states are implemented because we don't reference them
/// in the parser.
#[derive(Clone, Debug)]
pub struct EtmV4Decoder {
    /// The most recently broadcasted timestamp.
    pub timestamp: u64,
    /// As stated by the specification. The three latest addresses generated
    /// by the trace unit must be stored.
    pub addr_stack: Vec<(u64, AddrWidth)>,
    /// Remainig byte slice from the last feed function call.
    carry: Vec<u8>,
    /// The current speculation depth. Gets updated by recieved packets.
    pub curr_spec_depth: u32,
    /// This should be read from the TRCIDR8.MAXSPEC.
    pub max_spec_depth: u32,
    /// This is the cycle count packet output threshold.
    pub cc_threshold: u32,
}

impl Default for EtmV4Decoder {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

impl EtmV4Decoder {
    /// Creates a new instance of the trace analyzer.
    pub fn new(max_spec_depth: u32, cc_threshold: u32) -> Self {
        Self {
            timestamp: 0,
            addr_stack: Vec::new(),
            carry: Vec::new(),
            curr_spec_depth: 0,
            max_spec_depth,
            cc_threshold,
        }
    }

    // Finds the A-Sync index from the data. Returns zero if not found.
    // This should be done only once.
    fn find_async(data: &[u8]) -> usize {
        // Go until you find a 11 byte zero in the trace output.
        let iter = data.windows(11).enumerate();
        for (i, pattern) in iter {
            if pattern == [0; 11] {
                return i + 12;
            }
        }

        0
    }

    /// Tries to parse one packet
    fn parse_one(&mut self, data: &[u8]) -> ParseResult {
        if data.is_empty() {
            return ParseResult::NeedMore;
        }
        let b0 = data[0];

        match b0.into() {
            HdrClass::ExtensionPackets => {
                // We need the first payload byte to classify.
                if data.len() < 2 {
                    return ParseResult::NeedMore;
                }
                let payload = data[1];
                match payload {
                    0x0 => ParseResult::Item {
                        len: 12,
                        pkt: EtmV4Packet::Async,
                    },
                    0x3 => {
                        self.curr_spec_depth = 0;
                        ParseResult::Item {
                            len: 2,
                            pkt: EtmV4Packet::Discard,
                        }
                    }
                    0x5 => {
                        self.curr_spec_depth = 0;
                        ParseResult::Item {
                            len: 2,
                            pkt: EtmV4Packet::Overflow,
                        }
                    }
                    0x7 => ParseResult::Item {
                        len: 2,
                        pkt: EtmV4Packet::BranchFutureFlush,
                    },
                    _ => ParseResult::Skip { len: 2 },
                }
            }
            // FIXME: Should handle the byte appending in another function.
            HdrClass::TraceInfo => {
                let mut trace_info = TraceInfo::default();
                let mut pos = 1;

                // Only the first byte is of interest to us.
                let plctl = data[pos];
                // Run through PLCTL. Might be padding.
                while (data[pos] & (1 << 7)) != 0 {
                    pos += 1;
                }

                // info is present
                if (plctl & 0b1) != 0 {
                    match read_uleb128(&data[pos..]) {
                        UlebResult::NeedMore => return ParseResult::NeedMore,
                        UlebResult::Ok { value, len } => {
                            let info = value;
                            if (info & 0b1) != 0 {
                                trace_info.cc = true;
                            }
                            // A 3 bit value if not empty means that the
                            // conditional tracing is on. More explicit values
                            // is not done for now.
                            if (info & 0b1110) != 0 {
                                trace_info.cond = true;
                            }
                            if (info & 0b10000) != 0 {
                                trace_info.p0_load = true;
                            }
                            if (info & 0b100000) != 0 {
                                trace_info.p0_store = true;
                            }

                            pos += len;
                        }
                    }
                }

                // key is present
                if (plctl & 0b10) != 0 {
                    unimplemented!("data trace is not implemented for ETM");
                }

                // spec is present.
                if (plctl & 0b100) != 0 {
                    match read_uleb128(&data[pos..]) {
                        UlebResult::NeedMore => return ParseResult::NeedMore,
                        UlebResult::Ok { value, len } => {
                            trace_info.curr_spec_depth = value as u32;
                            self.curr_spec_depth = value as u32;
                            pos += len;
                        }
                    }
                }

                // cyct is present.
                if (plctl & 0b1000) != 0 {
                    match read_uleb128(&data[pos..]) {
                        UlebResult::NeedMore => return ParseResult::NeedMore,
                        UlebResult::Ok { value, len } => {
                            trace_info.cc_threshold = value as u16;
                            pos += len;
                        }
                    }
                }

                ParseResult::Item {
                    len: pos + 1,
                    pkt: EtmV4Packet::TraceInfo(trace_info),
                }
            }
            HdrClass::TraceOn => ParseResult::Item {
                len: 1,
                pkt: EtmV4Packet::TraceOn,
            },
            // FIXME: Should edit the curr_spec_depth if it exceeds the
            // max_spec_depth.
            HdrClass::FunctionReturn => {
                let mut packets = Vec::new();
                packets.push(EtmV4Packet::FunctionReturn);
                if self.curr_spec_depth > self.max_spec_depth {
                    packets.push(EtmV4Packet::Commit(1));
                    packets.push(EtmV4Packet::BranchFutureFlush);
                    self.curr_spec_depth -= 1;
                }
                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            // The next packet may be an address one. Let the parser handle it.
            HdrClass::Exception => {
                if data.len() < 2 {
                    return ParseResult::NeedMore;
                }
                let mut packets = Vec::new();
                let mut len = 1;

                let c = ((data[len] >> 7) & 0b1) != 0;
                if c {
                    packets.push(EtmV4Packet::Exception(Exception::new(&data[len..2])));
                    len += 1;
                } else {
                    // Should not happen for A/M cores.
                    packets.push(EtmV4Packet::Exception(Exception::new(&data[len..=2])));
                    len += 2;
                }

                if self.curr_spec_depth > self.max_spec_depth {
                    packets.push(EtmV4Packet::Commit(1));
                    self.curr_spec_depth -= 1;
                }

                packets.push(EtmV4Packet::Commit(1));
                // FIXME: Requires attention to the exception type.
                packets.push(EtmV4Packet::BranchFutureFlush);

                ParseResult::Multiple { len, pkts: packets }
            }
            HdrClass::ExceptionReturn => {
                let mut packets = Vec::new();

                packets.push(EtmV4Packet::ExceptionReturn);

                if self.curr_spec_depth > self.max_spec_depth {
                    self.curr_spec_depth -= 1;
                }

                packets.push(EtmV4Packet::BranchFutureFlush);

                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            HdrClass::AddressWithContext => {
                // Check the IS type.
                match data[0] {
                    0x82 => {
                        // IS0
                        if data.len() < 5 {
                            return ParseResult::NeedMore;
                        }
                        let b1 = data[1] as u32;
                        let b2 = data[2] as u32;
                        let b3 = data[3] as u32;
                        let b4 = data[4] as u32;
                        let addr = ((b1 & 0x7F) << 2) | (b2 << 9) | (b3 << 16) | (b4 << 24);
                        self.addr_stack.push((addr as u64, AddrWidth::W32));
                        if data[5] != 0 {
                            unimplemented!("There is additional context");
                        }
                        ParseResult::Item {
                            len: 6,
                            pkt: EtmV4Packet::Addr32(addr.into()),
                        }
                    }
                    0x83 => {
                        if data.len() < 5 {
                            return ParseResult::NeedMore;
                        }
                        let b1 = data[1] as u32;
                        let b2 = data[2] as u32;
                        let b3 = data[3] as u32;
                        let b4 = data[4] as u32;
                        let addr = ((b1 & 0x7F) << 1) | (b2 << 8) | (b3 << 16) | (b4 << 24);
                        self.addr_stack.push((addr as u64, AddrWidth::W32));
                        if data[5] != 0 {
                            unimplemented!("There is additional context");
                        }
                        ParseResult::Item {
                            len: 6,
                            pkt: EtmV4Packet::Addr32(addr.into()),
                        }
                    }
                    _ => ParseResult::Skip { len: 1 },
                }
            }
            // Addresses we actually expose
            HdrClass::LongAddress32 => {
                match data[0] {
                    // IS0: A[8:2] in b1[6:0], A[16:9] in b2[7:0], then A[23:16], A[31:24]
                    0x9A => {
                        if data.len() < 5 {
                            return ParseResult::NeedMore;
                        }
                        let b1 = data[1] as u32;
                        let b2 = data[2] as u32;
                        let b3 = data[3] as u32;
                        let b4 = data[4] as u32;
                        let addr = ((b1 & 0x7F) << 2) | (b2 << 9) | (b3 << 16) | (b4 << 24);
                        self.addr_stack.push((addr as u64, AddrWidth::W32));
                        ParseResult::Item {
                            len: 5,
                            pkt: EtmV4Packet::Addr32(addr.into()),
                        }
                    }
                    // IS1: A[7:1] in b1[6:0], A[15:8] in b2[7:0], then A[23:16], A[31:24]
                    0x9B => {
                        if data.len() < 5 {
                            return ParseResult::NeedMore;
                        }
                        let b1 = data[1] as u32;
                        let b2 = data[2] as u32;
                        let b3 = data[3] as u32;
                        let b4 = data[4] as u32;
                        let addr = ((b1 & 0x7F) << 1) | (b2 << 8) | (b3 << 16) | (b4 << 24);
                        self.addr_stack.push((addr as u64, AddrWidth::W32));
                        ParseResult::Item {
                            len: 5,
                            pkt: EtmV4Packet::Addr32(addr.into()),
                        }
                    }
                    _ => ParseResult::Skip { len: 1 },
                }
            }
            HdrClass::LongAddress64 => {
                if data.len() < 9 {
                    return ParseResult::NeedMore;
                }
                let mut v = [0u8; 8];
                v.copy_from_slice(&data[1..9]);
                let addr = u64::from_le_bytes(v) & !1u64;
                self.addr_stack.push((addr, AddrWidth::W64));
                ParseResult::Item {
                    len: 9,
                    pkt: EtmV4Packet::Addr64(addr),
                }
            }

            // ShortAddress packets per ETMv4 spec (0x95: IS0, 0x96: IS1)
            // Uses address_regs[0] as base and updates fields depending on continuation C.
            HdrClass::ShortAddress => {
                let last_addr = self.addr_stack.get(self.addr_stack.len());
                match (data.get(1).copied(), last_addr) {
                    (None, _) => ParseResult::NeedMore,
                    (Some(b1), last) => {
                        let c = (b1 >> 7) != 0;
                        let a_lo = b1 as u64;
                        let need_second = c;
                        if need_second && data.len() < 3 {
                            return ParseResult::NeedMore;
                        }
                        let a_hi = if need_second { data[2] as u64 } else { 0 };

                        // If no known base address,
                        // consume properly but do not emit.
                        if last.is_none() {
                            return ParseResult::Skip {
                                len: 1 + 1 + (need_second as usize),
                            };
                        }
                        let (base, width) = *last.unwrap();

                        match data[0] {
                            0x95 => {
                                // IS = 0, clear [8:0] (0x1FF)
                                // and set [8:2] from A bits
                                let mut addr = base & !0x1FFu64;
                                addr |= (a_lo << 2) & 0x1FC;
                                if need_second {
                                    addr &= !0x1FE00u64;
                                    addr |= (a_hi << 9) & 0x1FE00;
                                }
                                self.addr_stack.push((addr, width));
                                let pkt = match width {
                                    AddrWidth::W32 => EtmV4Packet::Addr32(addr),
                                    AddrWidth::W64 => EtmV4Packet::Addr64(addr),
                                };
                                ParseResult::Item {
                                    len: 1 + 1 + (need_second as usize),
                                    pkt,
                                }
                            }
                            0x96 => {
                                // IS = 1, clear [7:0] (0xFF) and set [7:1] from A bits
                                let mut addr = base & !0xFFu64;
                                addr |= a_lo << 1;
                                if need_second {
                                    addr &= !0xFF00u64;
                                    addr |= a_hi << 8;
                                }
                                self.addr_stack.push((addr, width));
                                let pkt = match width {
                                    AddrWidth::W32 => EtmV4Packet::Addr32(addr),
                                    AddrWidth::W64 => EtmV4Packet::Addr64(addr),
                                };
                                ParseResult::Item {
                                    len: 1 + 1 + (need_second as usize),
                                    pkt,
                                }
                            }
                            _ => ParseResult::Skip { len: 1 },
                        }
                    }
                }
            }
            HdrClass::Context => {
                // FIXME: handle this correctly
                if data[0] == 0x81 {
                    // We have a payload.
                    return ParseResult::Skip { len: 2 };
                }
                ParseResult::Skip { len: 1 }
            }

            HdrClass::ExactMatchAddress => ParseResult::Item {
                len: 1,
                pkt: EtmV4Packet::Addr32(self.addr_stack[self.addr_stack.len()].0),
            },
            // FIXME: Add timestamps.
            HdrClass::TimestampMarker => ParseResult::Skip { len: 1 },
            HdrClass::Timestamp => {
                // Check whether the N bit is set. This indicates dual packet
                let is_cycle = (data[0] & 0b1) != 0;
                let mut packets = Vec::new();
                let mut skip_len = 1;
                let timestamp = match read_uleb128(&data[skip_len..]) {
                    UlebResult::NeedMore => return ParseResult::NeedMore,
                    UlebResult::Ok { value, len } => {
                        self.timestamp = value;
                        skip_len += len;
                        value
                    }
                };
                let timestamp = EtmV4Packet::Timestamp(timestamp);
                packets.push(timestamp);
                // Means a cycle packet is present. Zero means unknown.
                if is_cycle {
                    let cycle = match read_uleb128(&data[skip_len..]) {
                        UlebResult::NeedMore => return ParseResult::NeedMore,
                        UlebResult::Ok { value, len } => {
                            skip_len += len;
                            value
                        }
                    };
                    packets.push(EtmV4Packet::CycleCount(cycle as u32));
                }
                ParseResult::Multiple {
                    len: skip_len,
                    pkts: packets,
                }
            }
            HdrClass::CycleCountFmt1 => {
                if data.len() < 2 {
                    return ParseResult::NeedMore;
                }
                let mut packets = Vec::new();
                let mut len = 0;
                let mut unknown = false;
                let mut commit: u32 = 0;
                let mut cyc_count: u32 = 0;

                if data[len] & 0b1 != 0 {
                    unknown = true;
                }

                len += 1;
                match read_uleb128(&data[len..]) {
                    UlebResult::Ok {
                        value,
                        len: commit_len,
                    } => {
                        len += commit_len;
                        commit += value as u32;
                    }
                    UlebResult::NeedMore => {
                        return ParseResult::NeedMore;
                    }
                }
                if !unknown {
                    match read_uleb128(&data[len..]) {
                        UlebResult::Ok {
                            value,
                            len: cyc_len,
                        } => {
                            len += cyc_len;
                            cyc_count += value as u32;
                        }
                        UlebResult::NeedMore => {
                            return ParseResult::NeedMore;
                        }
                    }
                }

                packets.push(EtmV4Packet::CycleCount(cyc_count));
                packets.push(EtmV4Packet::Commit(commit));

                ParseResult::Multiple { len, pkts: packets }
            }
            HdrClass::CycleCountFmt2 => {
                if data.len() < 2 {
                    return ParseResult::NeedMore;
                }

                let mut packets = Vec::new();

                let commit_count = if data[0] & 0b1 != 0 {
                    (((data[1] >> 4) & 0b1111) - 15) as u32 + self.max_spec_depth
                } else {
                    (((data[1] >> 4) & 0b1111) + 1).into()
                };
                let cyc_count = (data[1] & 0b1111) + self.cc_threshold as u8;
                if commit_count > 0 {
                    packets.push(EtmV4Packet::Commit(commit_count));
                }
                self.curr_spec_depth -= commit_count;

                packets.push(EtmV4Packet::CycleCount(cyc_count.into()));

                ParseResult::Multiple {
                    len: 2,
                    pkts: packets,
                }
            }
            HdrClass::CycleCountFmt3 => {
                let packets = vec![
                    EtmV4Packet::CycleCount(
                        ((data[0] & 0b11) + 1 + self.cc_threshold as u8).into(),
                    ),
                    EtmV4Packet::Commit((((data[0] >> 2) & 0b11) + 1).into()),
                ];
                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            HdrClass::Commit => {
                let mut skip_len = 1;
                let commit = match read_uleb128(&data[skip_len..]) {
                    UlebResult::NeedMore => return ParseResult::NeedMore,
                    UlebResult::Ok { value, len } => {
                        skip_len += len;
                        self.curr_spec_depth -= value as u32;
                        value as u32
                    }
                };

                ParseResult::Item {
                    len: skip_len,
                    pkt: EtmV4Packet::Commit(commit),
                }
            }
            HdrClass::CancelFmt1 => {
                let mut skip_len = 1;
                let mut packets = Vec::new();
                let cancel = match read_uleb128(&data[skip_len..]) {
                    UlebResult::NeedMore => return ParseResult::NeedMore,
                    UlebResult::Ok { value, len } => {
                        skip_len += len;
                        self.curr_spec_depth -= value as u32;
                        value as u32
                    }
                };

                packets.push(EtmV4Packet::Cancel(cancel));

                if (data[0] & 0b1) != 0 {
                    packets.push(EtmV4Packet::Mispredict);
                    packets.push(EtmV4Packet::ConditionalFlush);
                }

                ParseResult::Multiple {
                    len: skip_len,
                    pkts: packets,
                }
            }
            HdrClass::CancelFmt2 => {
                let mut packets = Vec::new();
                match data[0] & 0b11 {
                    0b01 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![true],
                        });
                    }
                    0b10 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![true, true],
                        });
                    }
                    0b11 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![false],
                        });
                    }
                    _ => {}
                }
                packets.push(EtmV4Packet::Cancel(1));
                self.curr_spec_depth -= 1;
                packets.push(EtmV4Packet::Mispredict);
                packets.push(EtmV4Packet::ConditionalFlush);

                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            HdrClass::CancelFmt3 => {
                let mut packets = Vec::new();
                if (data[0] & 0b1) != 0 {
                    packets.push(EtmV4Packet::Atom {
                        outcomes: vec![true],
                    })
                }

                packets.push(EtmV4Packet::Cancel(1));

                self.curr_spec_depth -= ((data[0] >> 2 & 0b11) + 2) as u32;

                packets.push(EtmV4Packet::Mispredict);
                packets.push(EtmV4Packet::ConditionalFlush);

                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            HdrClass::Mispredict => {
                let mut packets = Vec::new();
                match data[0] & 0b11 {
                    0b01 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![true],
                        });
                    }
                    0b10 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![true, true],
                        });
                    }
                    0b11 => {
                        packets.push(EtmV4Packet::Atom {
                            outcomes: vec![false],
                        });
                    }
                    _ => {}
                }

                packets.push(EtmV4Packet::Mispredict);
                packets.push(EtmV4Packet::ConditionalFlush);

                ParseResult::Multiple {
                    len: 1,
                    pkts: packets,
                }
            }
            // Q and anything else → skip 1 byte
            // (we’re not doing data tracing / Q reconstruction)
            HdrClass::Ignore | HdrClass::Event | HdrClass::Q | HdrClass::ReservedOrUnknown => {
                ParseResult::Skip { len: 1 }
            }
            // Atoms
            HdrClass::AtomFmt1
            | HdrClass::AtomFmt2
            | HdrClass::AtomFmt3
            | HdrClass::AtomFmt4
            | HdrClass::AtomFmt5
            | HdrClass::AtomFmt6 => Self::handle_atom(b0),
            _ => unimplemented!("unknown packet {:b} {:?}", b0, Into::<HdrClass>::into(b0)),
        }
    }

    fn handle_atom(b0: u8) -> ParseResult {
        match b0.into() {
            HdrClass::AtomFmt1 => {
                let e = (b0 & 0x01) != 0;
                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom { outcomes: vec![e] },
                }
            }
            HdrClass::AtomFmt2 => {
                let a0 = (b0 & 0x01) != 0;
                let a1 = (b0 & 0x02) != 0;
                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom {
                        outcomes: vec![a0, a1],
                    },
                }
            }
            HdrClass::AtomFmt3 => {
                let a0 = (b0 & 0x01) != 0;
                let a1 = (b0 & 0x02) != 0;
                let a2 = (b0 & 0x04) != 0;
                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom {
                        outcomes: vec![a0, a1, a2],
                    },
                }
            }
            HdrClass::AtomFmt4 => {
                let code = b0 & 0x03;
                let seq = match code {
                    0b00 => vec![false, true, true, true],
                    0b01 => vec![false, false, false, false],
                    0b10 => vec![false, true, false, true],
                    0b11 => vec![true, false, true, false],
                    _ => unreachable!(),
                };
                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom { outcomes: seq },
                }
            }
            HdrClass::AtomFmt5 => {
                let abc = (b0 & 0b11) | ((b0 & 0b100000) >> 3);
                let seq = match abc {
                    0b000 => {
                        vec![false, true, true, true, true]
                    }
                    0b001 => {
                        vec![false, false, false, false, false]
                    }
                    0b010 => {
                        vec![false, true, false, true, false]
                    }
                    0b011 => {
                        vec![true, false, true, false, true]
                    }
                    // All other values are not permitted
                    _ => {
                        unreachable!("unpermitted value in ABC of AtomFmt5");
                    }
                };

                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom { outcomes: seq },
                }
            }
            HdrClass::AtomFmt6 => {
                let count = (b0 & 0b11111) + 2;
                let a = (b0 >> 5) != 0;
                let mut seq = vec![true; count.into()];

                if a {
                    seq.push(false);
                } else {
                    seq.push(true);
                }

                ParseResult::Item {
                    len: 1,
                    pkt: EtmV4Packet::Atom { outcomes: seq },
                }
            }
            // Technically no other trace element should reach this.
            _ => ParseResult::Skip { len: 1 },
        }
    }

    /// Feeds the byte slice into the parser.
    pub fn feed(&mut self, data: &[u8]) -> Vec<EtmPacket> {
        let mut out = Vec::new();
        // Find the A-Sync packet to synchronize the trace data.
        let idx = Self::find_async(data);
        self.carry.extend_from_slice(&data[idx..]);
        let mut idx = 0;

        while idx < self.carry.len() {
            let chunk: Vec<u8> = self.carry[idx..].to_vec();
            match self.parse_one(&chunk) {
                ParseResult::NeedMore => break,
                ParseResult::Skip { len } => {
                    idx += len;
                }
                ParseResult::Item { len, pkt } => {
                    out.push(EtmPacket::V4(pkt));
                    idx += len;
                }
                ParseResult::Multiple { len, pkts } => {
                    for pkt in pkts {
                        out.push(EtmPacket::V4(pkt));
                    }
                    idx += len;
                }
            }
        }

        if idx > 0 {
            // Drop every consumed packet.
            self.carry.drain(0..idx);
        }

        out
    }
}

#[derive(Debug, Clone, Copy)]
enum UlebResult {
    NeedMore,
    Ok { value: u64, len: usize },
}
fn read_uleb128(mut data: &[u8]) -> UlebResult {
    let mut val = 0u64;
    let mut shift = 0u32;
    let mut n = 0usize;
    while let Some(&b) = data.first() {
        val |= ((b & 0x7F) as u64) << shift;
        n += 1;
        data = &data[1..];
        if b & 0x80 == 0 {
            return UlebResult::Ok { value: val, len: n };
        }
        shift += 7;
        if shift >= 64 {
            break;
        }
    }
    UlebResult::NeedMore
}
