use std::fmt;

/// An iterator over a received bit stream.
#[derive(Clone)]
pub struct BitIter<'a> {
    buf: &'a [u8],
    total_bits: usize,
    current_bit: usize,
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
            total_bits,
            current_bit: 0,
        }
    }
}

impl Iterator for BitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.current_bit == self.total_bits {
            return None;
        }

        let current_bit = self.current_bit;
        let byte = current_bit / 8;
        let bit = current_bit % 8;

        let bit = self.buf[byte] & (1 << bit) != 0;
        self.current_bit += 1;

        Some(bit)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.total_bits - self.current_bit;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for BitIter<'_> {}

impl fmt::Debug for BitIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();
        write!(f, "BitIter({})", s)
    }
}

pub(crate) trait IteratorExt: Sized {
    fn collapse_bytes(self) -> ByteIter<Self>;
}

impl<I: Iterator<Item = bool>> IteratorExt for I {
    fn collapse_bytes(self) -> ByteIter<Self> {
        ByteIter { inner: self }
    }
}

pub(crate) struct ByteIter<I> {
    inner: I,
}

impl<I: Iterator<Item = bool>> Iterator for ByteIter<I> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        // Collapse 8 bits from `inner` into a byte (LSb first).
        let mut byte = 0;
        let mut has_data = false;
        for (pos, bit) in self.inner.by_ref().take(8).enumerate() {
            has_data = true;
            byte |= (bit as u8) << pos;
        }

        has_data.then_some(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_bytes() {
        fn collapse<const N: usize>(v: [bool; N]) -> Vec<u8> {
            v.into_iter().collapse_bytes().collect()
        }

        assert_eq!(collapse([]), [] as [u8; 0]);
        assert_eq!(collapse([true]), [0x01]);
        assert_eq!(collapse([false, true]), [0x02]);
        assert_eq!(collapse([true, false]), [0x01]);
        assert_eq!(collapse([false]), [0x00]);
        assert_eq!(collapse([false; 8]), [0x00]);
        assert_eq!(collapse([true; 8]), [0xFF]);
        assert_eq!(collapse([true; 7]), [0x7F]);
        assert_eq!(collapse([true; 9]), [0xFF, 0x01]);
    }

    #[test]
    fn bit_iter() {
        fn bit_iter<const N: usize>(b: [u8; N], num: usize) -> Vec<bool> {
            BitIter::new(&b, num).collect()
        }

        assert_eq!(bit_iter([], 0), Vec::<bool>::new());
        assert_eq!(bit_iter([0xFF], 0), Vec::<bool>::new());
        assert_eq!(bit_iter([0xFF, 0xFF], 0), Vec::<bool>::new());
        assert_eq!(bit_iter([0xFF], 1), [true]);
        assert_eq!(bit_iter([0x00], 1), [false]);
        assert_eq!(bit_iter([0x01], 1), [true]);
        assert_eq!(bit_iter([0x01], 2), [true, false]);
        assert_eq!(bit_iter([0x02], 2), [false, true]);
        assert_eq!(bit_iter([0x02], 3), [false, true, false]);

        assert_eq!(
            bit_iter([0x01, 0x01], 9),
            [true, false, false, false, false, false, false, false, true]
        );
    }
}
