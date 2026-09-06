use crate::probe::BitSequence;

#[derive(Clone, Debug, Default)]
pub enum Command {
    /// No command should be output.
    #[default]
    None,

    /// Output one or more (<= 7) TMS bits.
    TmsBits {
        bit_count: usize,
        tms_bits: u8,
        tdi: bool,
        capture: bool,
    },

    /// Output one or more (<= 7) TDI bits, without changing the TMS line.
    TdiBits {
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },

    /// Output one or more TDI bytes, followed by zero or more bits.
    TdiSequence {
        tdi_bytes: Vec<u8>,
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },
}

const MAX_TMS_BITS: usize = 6;

impl Command {
    /// Encode a TMS path with TDI held at one level.
    pub(crate) fn encode_tms_path(path: &[bool], tdi: bool) -> Vec<Self> {
        let mut commands = Vec::new();
        let mut index = 0;
        while index < path.len() {
            let chunk_len = (path.len() - index).min(MAX_TMS_BITS);
            let mut tms_bits = 0u8;
            for offset in 0..chunk_len {
                if path[index + offset] {
                    tms_bits |= 1 << offset;
                }
            }
            commands.push(Self::TmsBits {
                bit_count: chunk_len,
                tms_bits,
                tdi,
                capture: false,
            });
            index += chunk_len;
        }
        commands
    }

    /// Encode a TDI exchange with TMS low, except for an optional merged exit bit.
    pub(crate) fn encode_tdi_exchange(
        data: &BitSequence,
        merge_exit: bool,
        capture: bool,
    ) -> Vec<Self> {
        let bit_count = data.len();
        if bit_count == 0 {
            return Vec::new();
        }

        let data_bits = if merge_exit { bit_count - 1 } else { bit_count };
        let mut commands = encode_tdi_bits(data, 0, data_bits, capture);

        if merge_exit {
            let last_tdi = data[bit_count - 1];
            commands.push(Self::TmsBits {
                bit_count: 1,
                tms_bits: 1,
                tdi: last_tdi,
                capture: false,
            });
        }

        commands
    }

    /// Encode a raw sequence with one TMS value for every bit.
    pub(crate) fn encode_raw_sequence(tms: bool, data: &BitSequence, capture: bool) -> Vec<Self> {
        if data.is_empty() {
            return Vec::new();
        }

        if tms {
            encode_tms_high_sequence(data, capture)
        } else {
            Command::encode_tdi_exchange(data, false, capture)
        }
    }

    /// Encode idle TCK clocks with TMS and TDI low.
    pub(crate) fn encode_clock_tck(count: u32) -> Vec<Self> {
        let mut commands = Vec::new();
        let mut remaining = count as usize;
        while remaining > 0 {
            let chunk = remaining.min(MAX_TMS_BITS);
            commands.push(Self::TdiBits {
                bit_count: chunk,
                tdi_bits: 0,
                capture: false,
            });
            remaining -= chunk;
        }
        commands
    }

    fn with_capture(self, capture: bool) -> Self {
        match self {
            Self::TmsBits {
                bit_count,
                tms_bits,
                tdi,
                ..
            } => Self::TmsBits {
                bit_count,
                tms_bits,
                tdi,
                capture,
            },
            Self::TdiBits {
                bit_count,
                tdi_bits,
                ..
            } => Self::TdiBits {
                bit_count,
                tdi_bits,
                capture,
            },
            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                tdi_bits,
                ..
            } => Self::TdiSequence {
                tdi_bytes,
                bit_count,
                tdi_bits,
                capture,
            },
            other => other,
        }
    }

    /// Returns the number of bytes that will be output by this command.
    pub fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::TmsBits { .. } | Self::TdiBits { .. } => 3,

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } if *bit_count == 0 => 3 + tdi_bytes.len(),

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } if *bit_count == 7 => 3 + tdi_bytes.len() + 6,

            Self::TdiSequence { tdi_bytes, .. } => 3 + tdi_bytes.len() + 3,
        }
    }

    /// Appends the command to the given buffer.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::None => {}
            Self::TmsBits {
                tms_bits,
                tdi,
                capture,
                bit_count,
            } => {
                let tms_byte = tms_bits | ((*tdi as u8) << 7);
                let cap_bit = if *capture { 0x20 } else { 0 };

                out.extend_from_slice(&[0x4b | cap_bit, *bit_count as u8 - 1, tms_byte]);
            }

            Self::TdiBits {
                tdi_bits,
                capture,
                bit_count,
                ..
            } => {
                let cap_bit = if *capture { 0x20 } else { 0 };

                let mut tdi_bits = *tdi_bits;
                let mut bit_count = *bit_count as u8;

                if bit_count == 7 {
                    // Some FTDI chips have trouble with 7 bits, so output 6 bits first and 1 later.
                    out.extend_from_slice(&[0x1b | cap_bit, 5, tdi_bits]);

                    tdi_bits >>= 6;
                    bit_count -= 6;
                }
                out.extend_from_slice(&[0x1b | cap_bit, bit_count - 1, tdi_bits]);
            }

            Self::TdiSequence {
                tdi_bytes,
                tdi_bits,
                capture,
                bit_count,
                ..
            } => {
                let cap_bit = if *capture { 0x20 } else { 0 };

                let [n_low, n_high] = (tdi_bytes.len() as u16 - 1).to_le_bytes();
                out.extend_from_slice(&[0x19 | cap_bit, n_low, n_high]);
                out.extend_from_slice(tdi_bytes);

                let mut tdi_bits = *tdi_bits;
                let mut bit_count = *bit_count as u8;

                if bit_count > 0 {
                    if bit_count == 7 {
                        // Some FTDI chips have trouble with 7 bits
                        out.extend_from_slice(&[0x1b | cap_bit, 5, tdi_bits]);

                        tdi_bits >>= 6;
                        bit_count -= 6;
                    }

                    out.extend_from_slice(&[0x1b | cap_bit, bit_count - 1, tdi_bits]);
                }
            }
        }
    }

    /// Records the number of bits that should be read from each read byte.
    ///
    /// Essentially, the FTDI chip returns a byte for each command that reads data. Depending on
    /// the commands we issue, we need to read a different number of bits from each byte.
    pub fn add_captured_bits(&self, bits: &mut Vec<usize>) {
        let capture = match self {
            Self::None => false,

            Self::TmsBits { capture, .. }
            | Self::TdiBits { capture, .. }
            | Self::TdiSequence { capture, .. } => *capture,
        };

        if !capture {
            return;
        }

        match self {
            Self::None => {}
            Self::TmsBits { bit_count, .. } => bits.push(*bit_count),
            Self::TdiBits { bit_count, .. } => {
                Self::add_data_bits_to_captured_bits(bits, *bit_count);
            }
            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } => {
                Self::add_bytes_to_captured_bits(bits, tdi_bytes.len());
                Self::add_data_bits_to_captured_bits(bits, *bit_count);
            }
        }
    }

    fn add_data_bits_to_captured_bits(bits: &mut Vec<usize>, bit_count: usize) {
        if bit_count == 7 {
            bits.push(6);
            bits.push(1);
        } else if bit_count != 0 {
            bits.push(bit_count);
        }
    }

    fn add_bytes_to_captured_bits(bits: &mut Vec<usize>, byte_count: usize) {
        for _ in 0..byte_count {
            bits.push(8);
        }
    }
}

fn encode_tms_high_sequence(data: &BitSequence, capture: bool) -> Vec<Command> {
    let mut commands = Vec::new();
    let mut index = 0;
    while index < data.len() {
        let tdi = data[index];
        let mut run = 1usize;
        while index + run < data.len() && data[index + run] == tdi {
            run += 1;
        }
        let path: Vec<bool> = vec![true; run];
        let is_last_run = index + run == data.len();
        let mut run_commands = Command::encode_tms_path(&path, tdi);
        if capture
            && is_last_run
            && let Some(last) = run_commands.pop()
        {
            run_commands.push(last.with_capture(true));
        }
        commands.extend(run_commands);
        index += run;
    }
    commands
}

fn encode_tdi_bits(
    data: &BitSequence,
    start: usize,
    bit_count: usize,
    capture: bool,
) -> Vec<Command> {
    if bit_count == 0 {
        return Vec::new();
    }

    let mut commands = Vec::new();
    let full_bytes = bit_count / 8;
    let tail_bits = bit_count % 8;

    if full_bytes > 0 {
        let mut tdi_bytes = Vec::with_capacity(full_bytes);
        for byte_index in 0..full_bytes {
            let mut byte = 0u8;
            for bit_offset in 0..8 {
                if data[start + byte_index * 8 + bit_offset] {
                    byte |= 1 << bit_offset;
                }
            }
            tdi_bytes.push(byte);
        }

        let mut tail_value = 0u8;
        for bit_offset in 0..tail_bits {
            if data[start + full_bytes * 8 + bit_offset] {
                tail_value |= 1 << bit_offset;
            }
        }

        commands.push(Command::TdiSequence {
            tdi_bytes,
            bit_count: tail_bits,
            tdi_bits: tail_value,
            capture,
        });
    } else {
        let mut tdi_bits = 0u8;
        for bit_offset in 0..tail_bits {
            if data[start + bit_offset] {
                tdi_bits |= 1 << bit_offset;
            }
        }
        commands.push(Command::TdiBits {
            bit_count: tail_bits,
            tdi_bits,
            capture,
        });
    }

    commands
}

#[cfg(test)]
pub(crate) mod decoder {
    pub(crate) fn decode_tdi_bytes(bytes: &[u8], start: usize, bit_count: usize) -> Vec<bool> {
        let mut bits = Vec::with_capacity(bit_count);
        for bit_index in 0..bit_count {
            let byte = bytes[start + bit_index / 8];
            bits.push(byte & (1 << (bit_index % 8)) != 0);
        }
        bits
    }

    pub(crate) fn decode_commands_full(bytes: &[u8]) -> Vec<(bool, bool, bool)> {
        let mut triples = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            let opcode = bytes[index];
            if opcode == 0x87 {
                break;
            }

            let capture = opcode & 0x20 != 0;
            match opcode & 0xdf {
                0x4b => {
                    let bit_count = bytes[index + 1] as usize + 1;
                    let tms_byte = bytes[index + 2];
                    let tdi = tms_byte & 0x80 != 0;
                    let tms_bits = tms_byte & 0x7f;
                    for bit in 0..bit_count {
                        triples.push(((tms_bits >> bit) & 1 == 1, tdi, capture));
                    }
                    index += 3;
                }
                0x19 => {
                    let byte_count =
                        u16::from_le_bytes([bytes[index + 1], bytes[index + 2]]) as usize + 1;
                    index += 3;
                    let tdi_bits = decode_tdi_bytes(bytes, index, byte_count * 8);
                    for tdi in tdi_bits {
                        triples.push((false, tdi, capture));
                    }
                    index += byte_count;
                }
                0x1b => {
                    let bit_count = bytes[index + 1] as usize + 1;
                    let tdi_bits = bytes[index + 2];
                    for bit in 0..bit_count {
                        triples.push((false, (tdi_bits >> bit) & 1 == 1, capture));
                    }
                    index += 3;
                }
                _ => panic!("unknown MPSSE opcode {opcode:#x}"),
            }
        }
        triples
    }
}
