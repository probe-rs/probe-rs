//! This code was initially written by https://github.com/windelbouwman.
//! It was moved to the probe-rs project in accordance with him.
//!
//! Additions and fixes have been made thereafter.
//!
//! Trace protocol for the SWO pin.
//!
//! Refer to appendix D4 in the ARMv7-M architecture reference manual.
//! Also a good reference is itmdump.c from openocd:
//! https://github.com/arduino/OpenOCD/blob/master/contrib/itmdump.c

use std::collections::VecDeque;

use scroll::Pread;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TracePacket {
    /// A sync package to enable synchronization in the byte stream.
    Sync,

    Overflow,

    TimeStamp {
        tc: usize,
        ts: usize,
    },
    /// ITM trace data.
    ItmData {
        id: usize,
        payload: Vec<u8>,
    },
    /// Signalizes that an event counter wrapped.
    /// This can happen for underflowing or overflowing counters.
    /// Multiple bits can be set.
    /// If two wraps of the same counter happen in quick succession,
    /// the core MUST generate two separate packets.
    EventCounterWrapping {
        /// POSTCNT wrap.
        cyc: bool,
        /// FOLDCNT wrap.
        fold: bool,
        /// LSUCNT wrap.
        lsu: bool,
        /// SLEEPCNT wrap.
        sleep: bool,
        /// EXCCNT wrap.
        exc: bool,
        /// CPICNT wrap.
        cpi: bool,
    },
    /// Signalizes that an exception(interrupt) happended.
    ExceptionTrace {
        exception: ExceptionType,
        action: ExceptionAction,
    },
    /// Notifies about a new PC sample.
    PcSample {
        pc: u32,
    },
    /// Signalizes that a new data trace event was received.
    PcTrace {
        /// The id of the DWT unit.
        id: usize,
        value: u32,
    },
    /// Signalizes that a memory access happened.
    /// This can contain an u8, u16 or u32 value.
    MemoryTrace {
        /// The id of the DWT unit.
        id: usize,
        access_type: MemoryAccessType,
        value: u32,
    },
    AddressTrace {
        /// The id of the DWT unit.
        id: usize,
        address: u16,
    },
}

/// This enum denotes the exception action taken by the CPU and is explained in table D4-6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExceptionAction {
    Entered,
    Exited,
    Returned,
}

/// This enum denotes the type of exception(interrupt) table D4-6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExceptionType {
    Reset,
    Nmi,
    HardFault,
    MemManage,
    BusFault,
    UsageFault,
    SVCall,
    DebugMonitor,
    PendSV,
    SysTick,
    ExternalInterrupt(usize),
}

/// This enum denotes the type of memory access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MemoryAccessType {
    Read,
    Write,
}

/// Trace data decoder.
///
/// This is a sans-io style decoder.
/// See also: https://sans-io.readthedocs.io/how-to-sans-io.html
pub struct Decoder {
    incoming: VecDeque<u8>,
    packets: VecDeque<TracePacket>,
    state: DecoderState,
}

enum DecoderState {
    Header,
    Syncing(usize),
    ItmData {
        id: usize,
        payload: Vec<u8>,
        size: usize,
    },
    DwtData {
        discriminant: usize,
        payload: Vec<u8>,
        size: usize,
    },
    TimeStamp {
        tc: usize,
        ts: Vec<u8>,
    },
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            incoming: VecDeque::new(),
            packets: VecDeque::new(),
            state: DecoderState::Header,
        }
    }

    /// Feed trace data into the decoder.
    pub fn feed(&mut self, data: Vec<u8>) {
        self.incoming.extend(&data)
    }

    fn next_byte(&mut self) -> Option<u8> {
        self.incoming.pop_front()
    }

    /// Pull the next item from the decoder.
    pub fn pull(&mut self) -> Option<TracePacket> {
        // Process any bytes:
        self.process_incoming();
        self.packets.pop_front()
    }

    fn process_incoming(&mut self) {
        while let Some(b) = self.next_byte() {
            self.process_byte(b);
        }
    }

    fn process_byte(&mut self, b: u8) {
        match &self.state {
            DecoderState::Header => {
                self.decode_first_byte(b);
            }
            DecoderState::Syncing(amount) => {
                let amount = *amount;
                self.handle_sync_byte(b, amount);
            }
            DecoderState::ItmData { payload, size, id } => {
                let mut payload = payload.clone();
                let id = *id;
                let size = *size;
                payload.push(b);
                self.handle_itm(id, payload, size);
            }
            DecoderState::DwtData {
                payload,
                size,
                discriminant,
            } => {
                let mut payload = payload.clone();
                let discriminant = *discriminant;
                let size = *size;
                payload.push(b);
                // We pad the discriminant wit three (3) zeroes to have the same alignment as inside the header.
                // this way we can use the same numbers as the spec does when doing bitmatching.
                self.handle_dwt(discriminant << 3, payload, size);
            }
            DecoderState::TimeStamp { tc, ts } => {
                let tc = *tc;
                let ts = ts.clone();
                self.handle_timestamp(b, tc, ts);
            }
        }
    }

    fn emit(&mut self, packet: TracePacket) {
        self.packets.push_back(packet);
    }

    fn decode_first_byte(&mut self, header: u8) {
        // let header: u8 = 0;

        // Figure out what we are dealing with!
        // See table D4-2.
        if header == 0x70 {
            log::debug!("Overflow!");
            self.emit(TracePacket::Overflow);
        } else if header == 0x0 {
            log::info!("Sync!");
            self.state = DecoderState::Syncing(1);
        // Read ~5 zero bytes (0x00) followed by 0x80
        // TracePacket::Sync
        } else {
            // Check low 4 bits now.
            let nibble = header & 0xf;
            match nibble {
                0 => {
                    log::trace!("Timestamp!");
                    if header & 0x80 == 0 {
                        // Short form timestamp
                        let ts = ((header >> 4) & 0x7) as usize;
                        let tc = 0;
                        if ts == 0 {
                            log::warn!("Invalid short timestamp!");
                        } else {
                            self.emit(TracePacket::TimeStamp { tc, ts });
                        }
                        self.state = DecoderState::Header;
                    } else {
                        assert!(header & 0xc0 == 0xc0);
                        let tc = ((header >> 4) & 0x3) as usize;
                        self.state = DecoderState::TimeStamp { tc, ts: vec![] };
                    }
                }
                0x4 => {
                    log::info!("Reserverd");
                    // TODO: Implement! Do not put unimplemented!() here as it will crash the logger in some cases!
                }
                0x8 => {
                    log::info!("Extension!");
                    // TODO: Implement! Do not put unimplemented!() here as it will crash the logger in some cases!
                }
                x => {
                    match extract_size(x) {
                        Err(msg) => {
                            log::warn!("Bad size: {}", msg);
                            self.state = DecoderState::Header;
                        }
                        Ok(size) => {
                            let discriminant = (header >> 3) as usize;
                            if x & 0x4 == 0x4 {
                                // DWT source / hardware source
                                log::trace!("DWT data! {:?} bytes", size);
                                self.state = DecoderState::DwtData {
                                    discriminant,
                                    payload: vec![],
                                    size,
                                };
                            } else {
                                // ITM data
                                log::trace!("Software ITM data {:?} bytes", size);
                                self.state = DecoderState::ItmData {
                                    id: discriminant,
                                    payload: vec![],
                                    size,
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_sync_byte(&mut self, b: u8, amount: usize) {
        match b {
            0x0 => {
                if amount > 6 {
                    log::warn!("Too many zero bytes in sync packet.");
                    self.state = DecoderState::Header;
                } else {
                    self.state = DecoderState::Syncing(amount + 1);
                }
            }
            0x80 => {
                if amount == 5 {
                    self.emit(TracePacket::Sync);
                } else {
                    log::warn!("Invalid amount of zero bytes in sync packet.");
                }
                self.state = DecoderState::Header;
            }
            x => {
                log::warn!("Invalid character in sync packet stream: 0x{:02X}.", x);
                self.state = DecoderState::Header;
            }
        }
    }

    fn handle_timestamp(&mut self, b: u8, tc: usize, mut ts_bytes: Vec<u8>) {
        let continuation = (b & 0x80) > 0;
        ts_bytes.push(b & 0x7f);
        if continuation {
            self.state = DecoderState::TimeStamp { tc, ts: ts_bytes };
        } else {
            let mut ts = 0;
            ts_bytes.reverse();
            for ts_byte in ts_bytes {
                ts <<= 7;
                ts |= ts_byte as usize;
            }
            self.emit(TracePacket::TimeStamp { tc, ts });
            self.state = DecoderState::Header;
        }
    }

    fn handle_itm(&mut self, id: usize, payload: Vec<u8>, size: usize) {
        if payload.len() == size {
            self.emit(TracePacket::ItmData { id, payload });
            self.state = DecoderState::Header;
        } else {
            self.state = DecoderState::ItmData { id, payload, size }
        }
    }

    fn handle_dwt(&mut self, header: usize, payload: Vec<u8>, size: usize) {
        let discriminant = header >> 3;

        if payload.len() == size {
            match discriminant {
                0 => self.emit(TracePacket::EventCounterWrapping {
                    cyc: (payload[0] >> 5) & 1 == 1,
                    fold: (payload[0] >> 4) & 1 == 1,
                    lsu: (payload[0] >> 3) & 1 == 1,
                    sleep: (payload[0] >> 2) & 1 == 1,
                    exc: (payload[0] >> 1) & 1 == 1,
                    cpi: payload[0] & 1 == 1,
                }),
                1 => self.emit(TracePacket::ExceptionTrace {
                    exception: match ((payload[1] as u16 & 1) << 8) | payload[0] as u16 {
                        1 => ExceptionType::Reset,
                        2 => ExceptionType::Nmi,
                        3 => ExceptionType::HardFault,
                        4 => ExceptionType::MemManage,
                        5 => ExceptionType::BusFault,
                        6 => ExceptionType::UsageFault,
                        11 => ExceptionType::SVCall,
                        12 => ExceptionType::DebugMonitor,
                        14 => ExceptionType::PendSV,
                        15 => ExceptionType::SysTick,
                        0 | 7 | 8 | 9 | 10 | 13 => {
                            log::error!(
                                "A corrupt ITM packet was received and discarded: header={}, payload={:?}.",
                                header,
                                payload,
                            );
                            return;
                        },
                        n => ExceptionType::ExternalInterrupt(n as usize),
                    },
                    action: match (payload[1] >> 4) & 0b11 {
                        0b01 => ExceptionAction::Entered,
                        0b10 => ExceptionAction::Exited,
                        0b11 => ExceptionAction::Returned,
                        _ => {
                            log::error!(
                                "A corrupt ITM packet was received and discarded: header={}, payload={:?}.",
                                header,
                                payload,
                            );
                            return;
                        },
                    },
                }),
                2 => self.emit(TracePacket::PcSample {
                    // This unwrap is okay, as the size was validated beforehand.
                    pc: payload.pread(0).unwrap(),
                }),
                _ => {
                    // Get the DWT unit id.
                    let unit_id = header >> 4 & 0b11;

                    // Get the packet type.
                    let packet_type = header >> 6 & 0b11;

                    let packet = if packet_type == 0b01 {
                        if header >> 3 & 1 == 0 {
                            // We got a PC value packet.
                            TracePacket::PcTrace {
                                id: unit_id,
                                // This unwrap is okay, as the size was validated beforehand.
                                value: payload.pread(0).unwrap(),
                            }
                        } else {
                            // We got an address packet.
                            TracePacket::AddressTrace {
                                id: unit_id,
                                // This unwrap is okay, as the size was validated beforehand.
                                address: payload.pread(0).unwrap(),
                            }
                        }
                    } else if packet_type == 0b10 {
                        if header >> 3 & 1 == 0 {
                            // We got a data value packet for read access.
                            TracePacket::MemoryTrace {
                                id: unit_id,
                                access_type: MemoryAccessType::Read,
                                // This unwrap is okay, as the size was validated beforehand.
                                value: payload.pread(0).unwrap(),
                            }
                        } else {
                            // We got a data value packet for write access.
                            TracePacket::MemoryTrace {
                                id: unit_id,
                                access_type: MemoryAccessType::Write,
                                // This unwrap is okay, as the size was validated beforehand.
                                value: payload.pread(0).unwrap(),
                            }
                        }
                    } else {
                        log::error!(
                            "A corrupt ITM packet was received and discarded: header={}, payload={:?}.",
                            header,
                            payload,
                        );
                        return;
                    };

                    self.emit(packet);
                }
            };
            self.state = DecoderState::Header;
        } else {
            self.state = DecoderState::DwtData {
                discriminant,
                payload,
                size,
            }
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn extract_size(c: u8) -> Result<usize, String> {
    match c & 0b11 {
        0b01 => Ok(1),
        0b10 => Ok(2),
        0b11 => Ok(4),
        _ => Err("Invalid".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, MemoryAccessType, TracePacket};

    #[test]
    fn example_capture1() {
        // Example trace, containing ITM trace data, timestamps and DWT trace data.
        let trace_data: Vec<u8> = vec![
            3, 65, 0, 0, 0, 192, 204, 244, 109, 3, 66, 0, 0, 0, 192, 29, 3, 67, 0, 0, 0, 112, 71,
            86, 0, 0, 8, 112, 143, 226, 239, 127, 91, 240, 196, 8,
        ];

        let mut decoder = Decoder::new();

        decoder.feed(trace_data);
        assert_eq!(
            Some(TracePacket::ItmData {
                id: 0,
                payload: vec![65, 0, 0, 0]
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::TimeStamp { tc: 0, ts: 1800780 }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::ItmData {
                id: 0,
                payload: vec![66, 0, 0, 0]
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::TimeStamp { tc: 0, ts: 29 }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::ItmData {
                id: 0,
                payload: vec![67, 0, 0, 0]
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::PcTrace {
                id: 0,
                value: 0x8000056,
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::MemoryTrace {
                id: 0,
                access_type: MemoryAccessType::Write,
                value: 0x5B7FEFE2,
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::TimeStamp { tc: 3, ts: 1092 }),
            decoder.pull()
        );
        assert_eq!(None, decoder.pull());
    }

    #[test]
    fn example_capture2() {
        // Example trace, containing ITM trace data, timestamps and DWT trace data.
        let trace_data: Vec<u8> = vec![
            71, 68, 0, 0, 8, 135, 215, 2, 0, 0, 192, 161, 245, 109, 71, 72, 0, 0, 8, 112, 71, 96,
            0, 0, 8, 112, 143, 216, 2, 0, 0, 240, 197,
        ];

        let mut decoder = Decoder::new();

        decoder.feed(trace_data);
        assert_eq!(
            Some(TracePacket::PcTrace {
                id: 0,
                value: 0x8000044,
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::MemoryTrace {
                id: 0,
                access_type: MemoryAccessType::Read,
                value: 727,
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::TimeStamp { tc: 0, ts: 1800865 }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::PcTrace {
                id: 0,
                value: 0x8000048,
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::PcTrace {
                id: 0,
                value: 0x8000060,
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::MemoryTrace {
                id: 0,
                access_type: MemoryAccessType::Write,
                value: 728,
            }),
            decoder.pull()
        );
        assert_eq!(None, decoder.pull());
    }
}
