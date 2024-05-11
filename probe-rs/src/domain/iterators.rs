//! A collection of helper iterators to get consistent behavior around item handling.

/// Counts the items that are being iterated.
pub struct Count<'a, I>(I, &'a mut usize);

impl<I: Iterator> Iterator for Count<'_, I> {
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.0.next()?;
        *self.1 += 1;
        Some(item)
    }
}

pub trait IterExt: Sized {
    fn counting(self, counter: &mut usize) -> Count<Self>;
    fn clipping(self, threshold: usize, remainder: &mut usize) -> Clip<'_, Self>;
}

impl<I: Iterator> IterExt for I {
    fn counting(self, counter: &mut usize) -> Count<Self> {
        Count(self, counter)
    }

    fn clipping(self, threshold: usize, remainder: &mut usize) -> Clip<Self> {
        Clip {
            iter: self,
            threshold,
            remainder,
        }
    }
}

/// Works like [`Iterator::take`] but never leaves a remainder of only one element.
///
/// It either leaves 2 elements and more or exactly 0.
///
/// In case there is one element left after the threshold value, we also return the last
/// element in the next iteration.
///
/// This helps us to never write "and 1 more" with the reasoning that the space used for this
/// text, can be used for printing that one item.
///
/// NOTE: This iterator possibly executes sideeffects of the two items after the
/// threshold is reached.
pub struct Clip<'a, I> {
    iter: I,
    threshold: usize,
    remainder: &'a mut usize,
}

impl<'a, I: Iterator> Iterator for Clip<'a, I> {
    type Item = I::Item;

    #[inline]
    fn next(&mut self) -> Option<<I as Iterator>::Item> {
        if self.threshold != 0 {
            self.threshold -= 1;
            self.iter.next()
        } else {
            if let Some(next) = self.iter.next() {
                let empty = self.iter.next().is_none();

                if empty {
                    // This case can only be hit in one specific case that is destroyed
                    // after we return this (because we remove max 2 items from the iterator)
                    // thus, we can safely just return None in any other case without any
                    // additional tracking such as decrementing the counter further as in the
                    // next iteration one of the None branches is hit guaranteed.

                    return Some(next);
                }

                while self.iter.next().is_some() {
                    *self.remainder += 1;
                }
                *self.remainder += 2;
            } else {
                *self.remainder = 0;
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::IterExt;

    #[test_case(vec![1, 2, 3, 4, 5, 6], 4, vec![1, 2, 3, 4])]
    #[test_case(vec![1, 2, 3, 4, 5, 6], 5, vec![1, 2, 3, 4, 5, 6])]
    #[test_case(vec![1, 2, 3, 4, 5, 6], 6, vec![1, 2, 3, 4, 5, 6])]
    #[test_case(vec![1, 2, 3, 4, 5, 6], 7, vec![1, 2, 3, 4, 5, 6])]
    fn clipping(input: Vec<usize>, threshold: usize, expected: Vec<usize>) {
        let mut remainder = 0;

        let result: Vec<usize> = input
            .iter()
            .cloned()
            .clipping(threshold, &mut remainder)
            .collect();
        assert_eq!(result, expected);
        assert_eq!(remainder, input.len() - expected.len());
    }
}
