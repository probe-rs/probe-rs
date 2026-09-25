use probe_rs::MemoryInterface;

use crate::{DebugError, Variable};

/// Extension methods to simply parse a value into a number of bytes.
pub trait ValueExt: Sized {
    type Out;

    /// Parse the value from a string into a number of bytes.
    fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError>;

    /// Read the value from a [`VariableLocation`].
    fn read_from_location(
        variable: &crate::Variable,
        memory: &mut dyn crate::MemoryInterface,
    ) -> Result<Self, DebugError>;
}

macro_rules! impl_extensions {
    ($t:ty, $bytes:expr) => {
        impl ValueExt for $t {
            type Out = [u8; $bytes];

            fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError> {
                match ::parse_int::parse::<$t>(s) {
                    Ok(value) => Ok(<$t>::to_le_bytes(value)),
                    Err(e) => Err(DebugError::WarnAndContinue {
                        message: format!("Invalid data conversion from value: {s:?}. {e:?}"),
                    }),
                }
            }

            fn read_from_location(
                variable: &Variable,
                memory: &mut dyn MemoryInterface,
            ) -> Result<Self, DebugError> {
                let mut buff: Self::Out = [0u8; $bytes];
                variable.memory_location.read(&mut buff, memory)?;

                Ok(<$t>::from_le_bytes(buff))
            }
        }
    };
}

impl_extensions!(u8, 1);
impl_extensions!(i8, 1);
impl_extensions!(u16, 2);
impl_extensions!(i16, 2);
impl_extensions!(u32, 4);
impl_extensions!(i32, 4);
impl_extensions!(u64, 8);
impl_extensions!(i64, 8);
impl_extensions!(u128, 16);
impl_extensions!(i128, 16);

impl_extensions!(f32, 4);
impl_extensions!(f64, 8);
