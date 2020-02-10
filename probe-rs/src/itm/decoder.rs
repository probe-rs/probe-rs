//! This code was initially written by https://github.com/windelbouwman.
//! It was moved to the probe-rs project in accordance with him.
//! 
//! Additions and fixes have been made thereafter.
//! 
//! Trace protocol for the SWO pin.
//!
//! Refer to appendix E in the ARMv7-M architecture reference manual.
//! Also a good reference is itmdump.c from openocd:
//! https://github.com/arduino/OpenOCD/blob/master/contrib/itmdump.c

use std::collections::VecDeque;

#[derive(Debug, PartialEq)]
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

    DwtData {
        id: usize,
        payload: Vec<u8>,
    },
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
        id: usize,
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
            DecoderState::DwtData { payload, size, id } => {
                let mut payload = payload.clone();
                let id = *id;
                let size = *size;
                payload.push(b);
                self.handle_dwt(id, payload, size);
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
        // See table E-1
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
                    unimplemented!();
                }
                0x8 => {
                    log::info!("Extension!");
                    unimplemented!();
                }
                x => {
                    match extract_size(x) {
                        Err(msg) => {
                            log::warn!("Bad size: {}", msg);
                            self.state = DecoderState::Header;
                        }
                        Ok(size) => {
                            let id = (header >> 3) as usize;
                            if x & 0x4 == 0x4 {
                                // DWT source / hardware source
                                log::trace!("DWT data! {:?} bytes", size);
                                self.state = DecoderState::DwtData {
                                    id,
                                    payload: vec![],
                                    size,
                                };
                            } else {
                                // ITM data
                                log::trace!("Software ITM data {:?} bytes", size);
                                self.state = DecoderState::ItmData {
                                    id,
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

    fn handle_dwt(&mut self, id: usize, payload: Vec<u8>, size: usize) {
        if payload.len() == size {
            self.emit(TracePacket::DwtData { id, payload });
            self.state = DecoderState::Header;
        } else {
            self.state = DecoderState::DwtData { id, payload, size }
        }
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
    use super::{Decoder, TracePacket};

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
            Some(TracePacket::DwtData {
                id: 8,
                payload: vec![86, 0, 0, 8]
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::DwtData {
                id: 17,
                payload: vec![226, 239, 127, 91]
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
            Some(TracePacket::DwtData {
                id: 8,
                payload: vec![68, 0, 0, 8]
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::DwtData {
                id: 16,
                payload: vec![215, 2, 0, 0]
            }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::TimeStamp { tc: 0, ts: 1800865 }),
            decoder.pull()
        );
        assert_eq!(
            Some(TracePacket::DwtData {
                id: 8,
                payload: vec![72, 0, 0, 8]
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::DwtData {
                id: 8,
                payload: vec![96, 0, 0, 8]
            }),
            decoder.pull()
        );
        assert_eq!(Some(TracePacket::Overflow), decoder.pull());
        assert_eq!(
            Some(TracePacket::DwtData {
                id: 17,
                payload: vec![216, 2, 0, 0]
            }),
            decoder.pull()
        );
        assert_eq!(None, decoder.pull());
    }
}