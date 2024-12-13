//! Helper macros to implement an access port
//!

/// Defines a new typed access port register for a specific access port.
/// Takes
/// - type: The type of the port.
/// - name: The name of the constructed type for the register. Also accepts a doc comment to be added to the type.
/// - address: The address relative to the base address of the access port.
/// - fields: A list of fields of the register type.
/// - from: a closure to transform from an `u32` to the typed register.
/// - to: A closure to transform from they typed register to an `u32`.
#[macro_export]
macro_rules! define_ap_register {
    (
        $(#[$outer:meta])*
        name: $name:ident,
        address: $address:expr,
        fields: [$($(#[$inner:meta])*$field:ident: $type:ty$(,)?)*],
        from: $from_param:ident => $from:expr,
        to: $to_param:ident => $to:expr
    )
    => {
        $(#[$outer])*
        #[allow(non_snake_case)]
        #[allow(clippy::upper_case_acronyms)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            $($(#[$inner])*pub $field: $type,)*
        }

        impl $crate::architecture::arm::communication_interface::Register for $name {
            // ADDRESS is always the lower 4 bits of the register address.
            const ADDRESS: u8 = $address;
            const NAME: &'static str = stringify!($name);
        }

        impl TryFrom<u32> for $name {
            type Error = $crate::architecture::arm::communication_interface::RegisterParseError;

            fn try_from($from_param: u32) -> Result<$name, Self::Error> {
                $from
            }
        }

        impl From<$name> for u32 {
            fn from($to_param: $name) -> u32 {
                $to
            }
        }
    }
}

/// Defines a new typed access port.
#[macro_export]
macro_rules! define_ap {
    (
        $(#[$outer:meta])*
        $name:ident
    ) => {
        $(#[$outer])*
        #[derive(Clone, Debug)]
        pub struct $name {
            address: FullyQualifiedApAddress,
        }

        impl $name {
            #[doc = concat!("Creates a new ", stringify!($name), " with `address` as base address.")]
            pub const fn new(address: FullyQualifiedApAddress) -> Self {
                Self { address }
            }
        }

        impl AccessPortType for $name {
            fn ap_address(&self) -> &FullyQualifiedApAddress {
                &self.address
            }
        }
    };
}
