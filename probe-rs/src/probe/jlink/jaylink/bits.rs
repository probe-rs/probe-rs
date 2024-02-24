use std::fmt;

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

    /// Returns the number of bits left in `self`.
    pub fn bits_left(&self) -> usize {
        self.bits_left
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
        let mut empty = true;
        for pos in 0..8 {
            let bit = if let Some(bit) = self.inner.next() {
                bit
            } else {
                break;
            };
            empty = false;
            let mask = if bit { 1 } else { 0 } << pos;
            byte |= mask;
        }

        if empty {
            None
        } else {
            Some(byte)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_bytes() {
        fn collapse(v: Vec<bool>) -> Vec<u8> {
            v.into_iter().collapse_bytes().collect()
        }

        assert_eq!(collapse(vec![]), vec![]);
        assert_eq!(collapse(vec![true]), vec![0x01]);
        assert_eq!(collapse(vec![false, true]), vec![0x02]);
        assert_eq!(collapse(vec![true, false]), vec![0x01]);
        assert_eq!(collapse(vec![false]), vec![0x00]);
        assert_eq!(collapse(vec![false; 8]), vec![0x00]);
        assert_eq!(collapse(vec![true; 8]), vec![0xFF]);
        assert_eq!(collapse(vec![true; 7]), vec![0x7F]);
        assert_eq!(collapse(vec![true; 9]), vec![0xFF, 0x01]);
    }

    #[test]
    fn bit_iter() {
        fn bit_iter(b: Vec<u8>, num: usize) -> Vec<bool> {
            BitIter::new(&b, num).collect()
        }

        assert_eq!(bit_iter(vec![], 0), Vec::<bool>::new());
        assert_eq!(bit_iter(vec![0xFF], 0), Vec::<bool>::new());
        assert_eq!(bit_iter(vec![0xFF, 0xFF], 0), Vec::<bool>::new());
        assert_eq!(bit_iter(vec![0xFF], 1), vec![true]);
        assert_eq!(bit_iter(vec![0x00], 1), vec![false]);
        assert_eq!(bit_iter(vec![0x01], 1), vec![true]);
        assert_eq!(bit_iter(vec![0x01], 2), vec![true, false]);
        assert_eq!(bit_iter(vec![0x02], 2), vec![false, true]);
        assert_eq!(bit_iter(vec![0x02], 3), vec![false, true, false]);

        assert_eq!(
            bit_iter(vec![0x01, 0x01], 9),
            vec![true, false, false, false, false, false, false, false, true]
        );
    }

    #[test]
    fn bit_iter_split_off() {
        fn split(b: Vec<u8>, split: usize, total: usize) -> (Vec<bool>, Vec<bool>) {
            let mut bits = BitIter::new(&b, total);
            let first = bits.split_off(split);
            let (first, second) = (first.collect::<Vec<_>>(), bits.collect::<Vec<_>>());
            assert_eq!(first.len(), split);
            assert_eq!(second.len(), total - split);
            (first, second)
        }

        assert_eq!(split(vec![0xF0], 4, 8), (vec![false; 4], vec![true; 4]));
        assert_eq!(
            split(vec![0xF0], 5, 8),
            (vec![false, false, false, false, true], vec![true; 3])
        );
        assert_eq!(
            split(vec![0xF0], 3, 8),
            (vec![false; 3], vec![false, true, true, true, true])
        );

        assert_eq!(
            split(vec![0xF0, 0x01], 4, 9),
            (vec![false; 4], vec![true; 5])
        );
        assert_eq!(
            split(vec![0xF0, 0xFE], 4, 9),
            (vec![false; 4], vec![true, true, true, true, false])
        );

        assert_eq!(
            split(vec![0xFF, 0x01], 9, 16),
            (vec![true; 9], vec![false; 7])
        );

        assert_eq!(
            split(vec![0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55], 35, 64),
            (
                vec![
                    false, true, false, true, false, true, false, true, true, false, true, false,
                    true, false, true, false, false, true, false, true, false, true, false, true,
                    true, false, true, false, true, false, true, false, false, true, false
                ],
                vec![
                    true, false, true, false, true, true, false, true, false, true, false, true,
                    false, false, true, false, true, false, true, false, true, true, false, true,
                    false, true, false, true, false
                ]
            )
        );

        assert_eq!(
            split(vec![0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55], 40, 64),
            (
                vec![
                    false, true, false, true, false, true, false, true, true, false, true, false,
                    true, false, true, false, false, true, false, true, false, true, false, true,
                    true, false, true, false, true, false, true, false, false, true, false, true,
                    false, true, false, true,
                ],
                vec![
                    true, false, true, false, true, false, true, false, false, true, false, true,
                    false, true, false, true, true, false, true, false, true, false, true, false
                ]
            )
        );
    }

    #[test]
    fn bit_iter_split_off_large_test() {
        let mut iterator = BitIter::new(
            &[
                0xFF, 0x10, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x40, 0x04, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0x11, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x24, 0x08, 0x00, 0x00, 0x9E, 0xFF,
            ],
            208,
        );

        assert_eq!(
            iterator.split_off(4).collect::<Vec<bool>>(),
            vec![true, true, true, true]
        );
        assert_eq!(
            iterator.split_off(8).collect::<Vec<bool>>(),
            vec![true, true, true, true, false, false, false, false]
        );
        assert_eq!(
            iterator.split_off(3).collect::<Vec<bool>>(),
            vec![true, false, false]
        );
        assert_eq!(
            iterator.split_off(35).collect::<Vec<bool>>(),
            vec![
                false, true, false, false, false, false, false, false, false, true, true, true,
                true, true, true, true, true, true, true, true, true, true, true, true, true, true,
                true, true, true, true, true, true, true, true, true,
            ]
        );

        assert_eq!(
            iterator.split_off(4).collect::<Vec<bool>>(),
            vec![true, true, true, true]
        );
        assert_eq!(
            iterator.split_off(8).collect::<Vec<bool>>(),
            vec![true, true, false, false, false, false, false, false]
        );
        assert_eq!(
            iterator.split_off(3).collect::<Vec<bool>>(),
            vec![true, false, false]
        );

        for _ in 0..35 {
            iterator.next();
        }

        assert_eq!(
            iterator.split_off(4).collect::<Vec<bool>>(),
            vec![true, true, true, true]
        );
        assert_eq!(
            iterator.split_off(8).collect::<Vec<bool>>(),
            vec![true, true, true, true, true, true, true, true]
        );
        assert_eq!(
            iterator.split_off(3).collect::<Vec<bool>>(),
            vec![true, false, false]
        );
    }
}
