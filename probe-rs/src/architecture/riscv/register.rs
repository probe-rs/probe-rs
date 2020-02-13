macro_rules! data_register {
    ($i:ident, $addr:expr, $name:expr) => {
        struct $i(u32);

        impl DebugRegister for $i {
            const ADDRESS: u8 = $addr;
            const NAME: &'static str = $name;
        }

        impl From<$i> for u32 {
            fn from(register: $i) -> Self {
                register.0
            }
        }

        impl From<u32> for $i {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
    };

    (pub $i:ident, $addr:expr, $name:expr) => {
        pub struct $i(u32);

        impl DebugRegister for $i {
            const ADDRESS: u8 = $addr;
            const NAME: &'static str = $name;
        }

        impl From<$i> for u32 {
            fn from(register: $i) -> Self {
                register.0
            }
        }

        impl From<u32> for $i {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
    };
}
