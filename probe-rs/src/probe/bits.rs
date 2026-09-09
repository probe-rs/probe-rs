//! Bit sequences for probe output.

use bitvec::order::Lsb0;
use bitvec::slice::BitSlice;
use bitvec::vec::BitVec;
use std::fmt;
use std::ops::Index;

/// A bit sequence for SWJ and JTAG output.
///
/// The probe sends the bit at index 0 first.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct BitSequence(BitVec<u8, Lsb0>);

impl BitSequence {
    /// Create an empty sequence.
    pub fn new() -> Self {
        Self(BitVec::new())
    }

    /// Create an empty sequence with space for `bits` bits.
    pub fn with_capacity(bits: usize) -> Self {
        Self(BitVec::with_capacity(bits))
    }

    /// Return the number of bits in the sequence.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Return whether the sequence has no bits.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Build a sequence from the low `len` bits of `bits`.
    pub fn from_u64(len: usize, bits: u64) -> Self {
        assert!(len <= 64, "from_u64 supports at most 64 bits");
        let bytes = bits.to_le_bytes();
        Self::from_bytes(&bytes, len)
    }

    /// Build a sequence from `bytes`, using `len` bits in LSB-first byte order.
    pub fn from_bytes(bytes: &[u8], len: usize) -> Self {
        let mut sequence = Self::with_capacity(len);
        for byte in bytes {
            for i in 0..8 {
                if sequence.len() == len {
                    return sequence;
                }
                sequence.0.push(byte & (1 << i) != 0);
            }
        }
        sequence
    }

    /// Build a sequence that repeats one bit value.
    pub fn repeat(bit: bool, len: usize) -> Self {
        Self(BitVec::repeat(bit, len))
    }

    /// Append one bit to the sequence.
    pub fn push(&mut self, bit: bool) {
        self.0.push(bit);
    }

    /// Append another sequence to this one.
    pub fn extend(&mut self, other: &BitSequence) {
        self.0.extend_from_bitslice(other.as_bits());
    }

    /// Return a subsequence of `len` bits starting at `start`.
    pub fn slice(&self, start: usize, len: usize) -> Self {
        Self(self.0[start..start + len].to_bitvec())
    }

    /// Iterate over the bits in send order.
    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        self.0.iter().map(|bit| *bit)
    }

    /// Return the underlying bit slice.
    pub fn as_bits(&self) -> &BitSlice<u8, Lsb0> {
        self.0.as_bitslice()
    }
}

impl Index<usize> for BitSequence {
    type Output = bool;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl fmt::Debug for BitSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BitSequence(")?;
        for bit in self.iter() {
            f.write_str(if bit { "1" } else { "0" })?;
        }
        write!(f, ", len={})", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64_sends_low_bit_first() {
        let sequence = BitSequence::from_u64(4, 0b1011);
        assert_eq!(sequence.len(), 4);
        assert_eq!(
            sequence.iter().collect::<Vec<_>>(),
            [true, true, false, true]
        );
    }

    #[test]
    fn from_bytes_sends_lsb_of_each_byte_first() {
        let sequence = BitSequence::from_bytes(&[0b1011_0100, 0b0000_0011], 10);
        assert_eq!(sequence.len(), 10);
        assert_eq!(
            sequence.iter().collect::<Vec<_>>(),
            [
                false, false, true, false, true, true, false, true, true, true,
            ]
        );
    }

    #[test]
    fn repeat_builds_a_sequence_of_one_bit() {
        let sequence = BitSequence::repeat(true, 51);
        assert_eq!(sequence.len(), 51);
        assert!(sequence.iter().all(|bit| bit));
    }

    #[test]
    fn sequence_longer_than_64_bits() {
        let mut sequence = BitSequence::from_u64(64, 0x8685_2D95_6209_F392);
        sequence.extend(&BitSequence::from_u64(64, 0x19BC_0EA2_E3DD_AFE9));
        assert_eq!(sequence.len(), 128);
    }

    #[test]
    #[should_panic(expected = "from_u64 supports at most 64 bits")]
    fn from_u64_panics_above_64_bits() {
        BitSequence::from_u64(65, 0);
    }
}
