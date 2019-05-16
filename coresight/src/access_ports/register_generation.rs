//! Helper macros to implement an access port
    
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

        impl APRegister<$port_type> for $name {
            // APBANKSEL is always the upper 4 bits of the register address
            const APBANKSEL: u8 = $address >> 4;
        }
    }
}

#[macro_export]
macro_rules! define_ap {
    ($name:ident) => {
        #[derive(Clone, Copy)]
        pub struct $name {
            port_number: u8,
        }

        impl $name {
            pub fn new(port_number: u8) -> Self {
                Self {
                    port_number
                }
            }
        }

        impl AccessPort for $name {
            fn get_port_number(&self) -> u8 {
                self.port_number
            }
        }
    }
}