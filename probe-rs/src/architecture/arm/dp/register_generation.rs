//! Helper macros to implement an access port
#[macro_export]
/// Defines a new debug port register for typed access.
macro_rules! define_dp_register {
    (
        $(#[$outer:meta])*
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
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $name {
            $(pub $field: $type,)*
        }

        impl Register for $name {
            // ADDRESS is always the lower 4 bits of the register address
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
    }
}
