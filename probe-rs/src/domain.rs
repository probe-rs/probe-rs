pub struct CountingIter<'a, I>(I, &'a mut usize);

impl<I: Iterator> Iterator for CountingIter<'_, I> {
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.0.next()?;
        *self.1 += 1;
        Some(item)
    }
}

trait IterExt: Sized {
    fn counting(self, counter: &mut usize) -> CountingIter<Self>;
}

impl<I: Iterator> IterExt for I {
    fn counting(self, counter: &mut usize) -> CountingIter<Self> {
        CountingIter(self, counter)
    }
}
