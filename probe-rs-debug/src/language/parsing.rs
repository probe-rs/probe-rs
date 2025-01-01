use crate::DebugError;

/// Extension methods to simply parse a value into a number of bytes.
pub trait ParseToBytes {
    type Out;

    fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError>;
}

macro_rules! impl_parse {
    ($t:ty, $bytes:expr) => {
        impl ParseToBytes for $t {
            type Out = [u8; $bytes];

            fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError> {
                match ::parse_int::parse::<$t>(s) {
                    Ok(value) => Ok(<$t>::to_le_bytes(value)),
                    Err(e) => Err(DebugError::WarnAndContinue {
                        message: format!("Invalid data conversion from value: {s:?}. {e:?}"),
                    }),
                }
            }
        }
    };
}

impl_parse!(u8, 1);
impl_parse!(i8, 1);
impl_parse!(u16, 2);
impl_parse!(i16, 2);
impl_parse!(u32, 4);
impl_parse!(i32, 4);
impl_parse!(u64, 8);
impl_parse!(i64, 8);
impl_parse!(u128, 16);
impl_parse!(i128, 16);

impl_parse!(f32, 4);
impl_parse!(f64, 8);
