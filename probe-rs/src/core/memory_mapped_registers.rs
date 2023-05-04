/// A memory mapped register, for instance ARM debug registers (DHCSR, etc).
pub trait MemoryMappedRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    /// The register's address in the target memory.
    const ADDRESS: u64;
    /// The register's name.
    const NAME: &'static str;
}
