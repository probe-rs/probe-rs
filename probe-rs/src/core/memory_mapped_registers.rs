#[derive(Debug, thiserror::Error)]
#[error("Overflow while attempting to determine the MMIO address for register {register} at offset {offset:#x} from base address {base_address:#x}")]
pub struct RegisterAddressOutOfBounds {
    register: &'static str,
    base_address: u64,
    offset: u64,
}

/// A memory mapped register, for instance ARM debug registers (DHCSR, etc).
pub trait MemoryMappedRegister<T>: Clone + From<T> + Into<T> + Sized + std::fmt::Debug {
    /// The register's address in the target memory.
    /// For some architectures (e.g. ARM Cortex-A) this address is offset from a base address.
    /// For others (e.g. ARM Cortex-M, RISC-V) this address is absolute.
    const ADDRESS_OFFSET: u64;
    /// The register's name.
    const NAME: &'static str;
    /// Get the register's address in the memory map.
    /// For architectures like ARM Cortex-A, this address is offset from a base address, which must be supplied when calling this method.
    /// For others (e.g. ARM Cortex-M, RISC-V) where this address is a constant, please use [`MemoryMappedRegister::get_mmio_address`].
    fn get_mmio_address_from_base(base_address: u64) -> Result<u64, RegisterAddressOutOfBounds> {
        if let Some(mmio_address) = base_address.checked_add(Self::ADDRESS_OFFSET) {
            Ok(mmio_address)
        } else {
            Err(RegisterAddressOutOfBounds {
                register: Self::NAME,
                base_address,
                offset: Self::ADDRESS_OFFSET,
            })
        }
    }
    /// Get the register's address in the memory map.
    /// For architectures ARM Cortex-M and RISC-V, this address is constant value stored as part of [`MemoryMappedRegister::ADDRESS_OFFSET`].
    /// For other architectures (e.g. ARM Cortex-A) where this address is offset from a base address, please use [`MemoryMappedRegister::get_mmio_address_from_base`].
    fn get_mmio_address() -> u64 {
        Self::ADDRESS_OFFSET
    }
}

/// Create a [`MemoryMappedRegister`] type, with the required method implementations for:
/// - Trait implementations required by [`MemoryMappedRegister`]
/// - Includes a `bitfield!` mapping for bitfield access to optionally defined fields.
/// When no bitfields are defined, the default `.0` field must be used.
///
/// # Example
/// ```
/// use bitfield::bitfield;
/// use probe_rs::memory_mapped_bitfield_register;
/// memory_mapped_bitfield_register! {
///    /// Abstract Control and Status (see section xyz of some reference manual)
///    pub struct AbstractCS(u32);
///    0x16,"abstractcs",
///    impl From;
///    progbufsize, _: 28, 24;
///    busy, _: 12;
///    cmderr, set_cmderr: 10, 8;
///    datacount, _: 3, 0;
/// }
/// ```
/// This will generate a struct with the name `AbstractCS`, which has:
/// - A `pub` visibility.
/// - A `u32` register type.
/// - A `Debug` implementation.
/// - A `Copy` implementation.
/// - A `Clone` implementation.
/// - Default `From<u32>` and `From<MemoryMappedRegister>` impls for [`MemoryMappedRegister`] are generated.
/// - A `bitfield!` mapping for the fields `progbufsize`, `busy`, `cmderr`, `datacount`.
/// - `bitfield!` getters and setters for the fields as defined - See [`bitfield::bitfield!`] for more information.
/// - A `const ADDRESS_OFFSET: u64 = 0x16;`.
/// - A `const NAME: &'static str = "abstractcs";`.
macro_rules! memory_mapped_bitfield_register {
    ($(#[$outer:meta])* $visibility:vis struct $struct_name:ident($reg_type:ty); $addr:expr, $reg_name:expr, impl From; $($rest:tt)*) => {
        $crate::core::memory_mapped_registers::memory_mapped_bitfield_register!{
            $(#[$outer])* $visibility struct $struct_name($reg_type); $addr, $reg_name, $($rest)*
        }

        impl From<$struct_name> for $reg_type {
            fn from(register: $struct_name) -> Self {
                register.0
            }
        }

        impl From<$reg_type> for $struct_name {
            fn from(value: $reg_type) -> Self {
                Self(value)
            }
        }
    };
    ($(#[$outer:meta])* $vis_modifier:vis struct $struct_name:ident($reg_type:ty); $addr:expr, $reg_name:expr, $($rest:tt)*) => {
        // Using paste here, because as of bitfield = "0.14.0" they do not use the 'vis' specifier, and balks at being passed a visibility token.
        bitfield::bitfield!{
            $(#[$outer])*
            #[doc= concat!("A [`bitfield::bitfield!`] register mapping for the register `",  $reg_name, "` located at address `", stringify!($addr), "`.")]
            #[derive(Copy, Clone)]
            #[allow(clippy::upper_case_acronyms)]
            #[allow(non_camel_case_types)]
            ($vis_modifier) struct $struct_name($reg_type);
            impl Debug;
            $($rest)*
        }

        impl $crate::MemoryMappedRegister<$reg_type> for $struct_name {
            const ADDRESS_OFFSET: u64 = $addr;
            const NAME: &'static str = $reg_name;
        }
    };
}

pub(crate) use memory_mapped_bitfield_register;
