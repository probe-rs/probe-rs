pub trait Register: Clone + From<u32> + Into<u32> + Sized {
    const ADDRESS: u16;
}