//! Crate-public stuctures and utilites to be shared between probes.

use core::fmt;
use std::collections::VecDeque;

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

/// An iterator over a received bit stream.
#[derive(Clone)]
pub struct OwnedBitIter {
    buf: VecDeque<u8>,
    next_bit: u8,
    bits_left: usize,
}

impl OwnedBitIter {
    pub(crate) fn new(slice: &[u8], total_bits: usize) -> Self {
        assert!(
            slice.len() * 8 >= total_bits,
            "cannot pull {} bits out of {} bytes",
            total_bits,
            slice.len()
        );
        let mut buf = VecDeque::new();
        buf.extend(slice);
        Self {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }

    pub fn iter(&mut self) -> BitIter<'_> {
        self.buf.make_contiguous();
        BitIter::new(self.buf.as_slices().0, self.bits_left)
    }
}

impl Iterator for OwnedBitIter {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.bits_left > 0 {
            let byte = self.buf.iter().next().unwrap();
            let bit = byte & (1 << self.next_bit) != 0;
            if self.next_bit < 7 {
                self.next_bit += 1;
            } else {
                self.next_bit = 0;
                self.buf.pop_front();
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

impl FromIterator<bool> for OwnedBitIter {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = bool>,
    {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let mut buf = VecDeque::with_capacity(upper.unwrap_or(lower));
        let mut total_bits = 0;
        let mut current_byte = 0;
        let mut bit_index: u8 = 0;
        for b in iter {
            if b {
                current_byte |= 1 << bit_index
            }
            if bit_index < 7 {
                bit_index += 1;
            } else {
                buf.push_back(current_byte);
                current_byte = 0;
                bit_index = 0;
            }
            total_bits += 1;
        }
        if bit_index > 0 {
            buf.push_back(current_byte);
        }

        OwnedBitIter {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }
}

impl ExactSizeIterator for OwnedBitIter {}

impl fmt::Debug for OwnedBitIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();
        write!(f, "BitIter({s})")
    }
}

pub(crate) fn bits_to_byte(bits: impl IntoIterator<Item = bool>) -> u32 {
    let mut bit_val = 0u32;

    for (index, bit) in bits.into_iter().take(32).enumerate() {
        if bit {
            bit_val |= 1 << index;
        }
    }

    bit_val
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn owned_collect() {
        let one = [true, true, true, true, true, true, true, true, true];
        let two = [true, true, true, true, true, true, true, true, true];

        let bits = one.into_iter().chain(two);

        let s = bits
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();

        let x: OwnedBitIter = bits.clone().collect();

        println!("Actual: {}, Owned: {:?} : {:?}", s, x, x.buf);

        assert!(bits.eq(x))
    }
}
