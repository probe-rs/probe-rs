//! Crate-public stuctures and utilites to be shared between probes.

use core::fmt;

/// An iterator over a received bit stream.
#[derive(Clone)]
pub struct BitIter<'a> {
    buf: &'a [u8],
    next_bit: u8,
    bits_left: usize,
}

impl<'a> BitIter<'a> {
    pub(crate) fn new(buf: &'a [u8], total_bits: usize) -> Self {
        assert!(
            buf.len() * 8 >= total_bits,
            "cannot pull {} bits out of {} bytes",
            total_bits,
            buf.len()
        );

        Self {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }

    /// Splits off another `BitIter` from `self`s current position that will return `count` bits.
    ///
    /// After this call, `self` will be advanced by `count` bits.
    pub fn split_off(&mut self, count: usize) -> BitIter<'a> {
        assert!(count <= self.bits_left);
        let other = Self {
            buf: self.buf,
            next_bit: self.next_bit,
            bits_left: count,
        };

        // Update self
        let next_byte = (count + self.next_bit as usize) / 8;
        self.next_bit = (count as u8 + self.next_bit) % 8;
        self.buf = &self.buf[next_byte..];
        self.bits_left -= count;
        other
    }
}

impl Iterator for BitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.bits_left > 0 {
            let byte = self.buf.first().unwrap();
            let bit = byte & (1 << self.next_bit) != 0;
            if self.next_bit < 7 {
                self.next_bit += 1;
            } else {
                self.next_bit = 0;
                self.buf = &self.buf[1..];
            }

            self.bits_left -= 1;
            Some(bit)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.bits_left, Some(self.bits_left))
    }
}

impl ExactSizeIterator for BitIter<'_> {}

impl fmt::Debug for BitIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();
        write!(f, "BitIter({s})")
    }
}
