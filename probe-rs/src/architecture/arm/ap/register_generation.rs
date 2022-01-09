//! Helper macros to implement an access port
//!

#[macro_export]
macro_rules! define_ap_register {
    (
        $(#[$outer:meta])*
        $port_type:ident,
        $name:ident,
        $address:expr,
        [$(($field:ident: $type:ty)$(,)?)*],
        $param:ident,
        $from:expr,
        $to:expr
    )
    => {
        $(#[$outer])*
        #[allow(non_snake_case)]
        #[allow(clippy::upper_case_acronyms)]
        #[derive(Debug, Default, Clone, Copy, PartialEq)]
        pub struct $name {
            $(pub $field: $type,)*
        }

        impl Register for $name {
            // ADDRESS is always the lower 4 bits of the register address.
            const ADDRESS: u8 = $address;
            const NAME: &'static str = stringify!($name);
        }

        impl From<u32> for $name {
            fn from($param: u32) -> $name {
                $from
            }
        }

        impl From<$name> for u32 {
            fn from($param: $name) -> u32 {
                $to
            }
        }

        impl ApRegister<$port_type> for $name {
        }
    }
}

macro_rules! define_ap {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug)]
        pub struct $name {
            address: ApAddress,
        }

        impl $name {
            pub const fn new(address: ApAddress) -> Self {
                Self { address }
            }
        }

        impl AccessPort for $name {
            fn ap_address(&self) -> ApAddress {
                self.address
            }
        }
    };
}
