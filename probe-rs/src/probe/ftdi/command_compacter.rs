#[derive(Clone, Debug)]
pub enum Command {
    /// No command should be output.
    ///
    /// This state remembers the last state of the TMS line.
    None { tms: bool },

    /// Output one or more (<= 7) TMS bits.
    TmsBits {
        bit_count: usize,
        tms_bits: u8,
        tdi: bool,
        capture: bool,
    },

    /// Output one or more (<= 7) TDI bits, without changing the TMS line.
    ///
    /// There is an issue where exactly 7 bits need to be broken up into 2 commands. This
    /// implementation handles this case by outputting 6 bits in the first command, and the
    /// remaining 1 bit in the second command.
    TdiBits {
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },

    /// Output one or more TDI bytes, followed by zero or more bits.
    ///
    /// This is the most compact encoding to output a sequence of TDI bits.
    TdiSequence {
        tdi_bytes: Vec<u8>,
        bit_count: usize,
        tdi_bits: u8,
        capture: bool,
    },
}

impl Default for Command {
    fn default() -> Self {
        Self::None { tms: false }
    }
}

impl Command {
    fn start_new_command(&mut self, tms: bool, tdi: bool, capture: bool) -> Option<Self> {
        // We assume the case where TMS = 1 starts a state transition, and TMS = 0 is a
        // DR/IR scan. This may not always be true or optimal, but the end result only
        // differs in the number of bytes/commands in the buffer, so it is good enough.
        // For example, when we are in the Capture-DR state and want to enter Shift-DR,
        // this mechanism does the state transition as a TdiBits command.
        let tms_prev = self.last_tms();

        let new_command = if !tms && !tms_prev {
            Self::TdiBits {
                bit_count: 1,
                tdi_bits: tdi as u8,
                capture,
            }
        } else {
            Self::TmsBits {
                bit_count: 1,
                tms_bits: tms as u8,
                tdi,
                capture,
            }
        };

        let old = std::mem::replace(self, new_command);

        if matches!(old, Self::None { .. }) {
            None
        } else {
            Some(old)
        }
    }

    /// Appends a JTAG bit to the command.
    ///
    /// This function may return a finalised command in certain cases.
    pub fn append_jtag_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Option<Self> {
        match self {
            Self::None { .. } => self.start_new_command(tms, tdi, capture),

            Self::TmsBits {
                bit_count,
                tms_bits,
                tdi: tdi_prev,
                capture: capture_prev,
            } => {
                // The TMS writing command sets TDI to a fixed value specified in the upper bit.
                // This means this command can only output a single TDI bit, and we need to split
                // the command if the TDI bit changes.
                let same_tdi = *tdi_prev == tdi;

                // We need to output a different command if the capture bit changes.
                let same_capture = *capture_prev == capture;

                if same_tdi && same_capture {
                    // Data is shifted out LSB first, so add later bits to the upper bits.
                    *tms_bits |= (tms as u8) << *bit_count;
                    *bit_count += 1;

                    // Stop at 6 bits to sidestep the 7-bit issue which may or may not affect TMS
                    // shifts. We normally don't need more than 6 bits anyway.
                    if *bit_count == 6 { self.take() } else { None }
                } else {
                    // We need to start assembling a different command for one of the above reasons.
                    self.start_new_command(tms, tdi, capture)
                }
            }

            Self::TdiBits {
                bit_count,
                tdi_bits,
                capture: capture_prev,
            } => {
                // We need to output a different command if the capture bit changes.
                let same_capture = *capture_prev == capture;

                // Writing TDI bits assumes the TMS line is low, so we need to split the command
                // if the TMS line changes.
                if tms || !same_capture {
                    // We need to start assembling a different command for one of the above reasons.
                    return self.start_new_command(tms, tdi, capture);
                }

                // Data is shifted out LSB first, so add later bits to the upper bits.
                *tdi_bits |= (tdi as u8) << *bit_count;
                *bit_count += 1;

                // We have a full byte, transform the command into a TdiSequence.
                if *bit_count == 8 {
                    *self = Self::TdiSequence {
                        tdi_bytes: vec![*tdi_bits],
                        bit_count: 0,
                        tdi_bits: 0,
                        capture,
                    };
                }
                None
            }

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                tdi_bits,
                capture: capture_prev,
            } => {
                // We need to output a different command if the capture bit changes.
                let same_capture = *capture_prev == capture;

                // Writing TDI bits assumes the TMS line is low, so we need to split the command
                // if the TMS line changes.
                if tms || !same_capture {
                    // We need to start assembling a different command for one of the above reasons.
                    return self.start_new_command(tms, tdi, capture);
                }

                // Data is shifted out LSB first, so add later bits to the upper bits.
                *tdi_bits |= (tdi as u8) << *bit_count;
                *bit_count += 1;

                // We're done with a full byte, let's add it to the buffer.
                if *bit_count == 8 {
                    tdi_bytes.push(*tdi_bits);
                    *bit_count = 0;
                    *tdi_bits = 0;
                }

                None
            }
        }
    }

    /// Returns the number of bytes that will be output by this command.
    pub fn len(&self) -> usize {
        match self {
            Self::None { .. } => 0,
            Self::TmsBits { .. } | Self::TdiBits { .. } => 3,

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } if *bit_count == 0 => {
                // We output a sequence of full bytes only.
                3 + tdi_bytes.len()
            }

            Self::TdiSequence {
                tdi_bytes,
                bit_count,
                ..
            } if *bit_count == 7 => {
                // We output a sequence of full bytes, followed by 2 commands to output bits.
                3 + tdi_bytes.len() + 6
            }

            Self::TdiSequence { tdi_bytes, .. } => {
                // We output a sequence of full bytes, followed by one command to output bits.
                3 + tdi_bytes.len() + 3
            }
        }
    }

    /// Appends the command to the given buffer.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::None { .. } => {}
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

                // Append full bytes
                let [n_low, n_high] = (tdi_bytes.len() as u16 - 1).to_le_bytes();
                out.extend_from_slice(&[0x19 | cap_bit, n_low, n_high]);
                out.extend_from_slice(tdi_bytes);

                // Append remaining bits
                let mut tdi_bits = *tdi_bits;
                let mut bit_count = *bit_count as u8;

                if bit_count > 0 {
                    if bit_count == 7 {
                        // Some FTDI chips have trouble with 7 bits
                        // output 6 bits
                        out.extend_from_slice(&[0x1b | cap_bit, 5, tdi_bits]);

                        tdi_bits >>= 6;
                        bit_count -= 6;
                    }

                    out.extend_from_slice(&[0x1b | cap_bit, bit_count - 1, tdi_bits]);
                }
            }
        }
    }

    fn last_tms(&self) -> bool {
        match self {
            Self::None { tms } => *tms,

            Self::TmsBits {
                tms_bits,
                bit_count,
                ..
            } => (*tms_bits & (0x01 << (*bit_count - 1))) != 0,

            // We are pushing out data in which case the TMS line should be low.
            Self::TdiBits { .. } | Self::TdiSequence { .. } => false,
        }
    }

    /// Records the number of bits that should be read from each read byte.
    ///
    /// Essentially, the FTDI chip returns a byte for each command that reads data. Depending on
    /// the commands we issue, we need to read a different number of bits from each byte.
    pub fn add_captured_bits(&self, bits: &mut Vec<usize>) {
        let capture = match self {
            Self::None { .. } => false,

            Self::TmsBits { capture, .. }
            | Self::TdiBits { capture, .. }
            | Self::TdiSequence { capture, .. } => *capture,
        };

        if !capture {
            return;
        }

        match self {
            Self::None { .. } => {}
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

    /// Returns the current command if it is not empty, and resets the command to None.
    pub fn take(&mut self) -> Option<Self> {
        let this = std::mem::take(self);

        *self = Self::None {
            tms: this.last_tms(),
        };

        Some(this)
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
