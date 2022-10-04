use serde::Serializer;

/// This trait is used by `probe-rs-target` to constrain the serialization of numbers to hex strings, to be generic for unsigned integers.
pub trait SerializeUnsignedInt {
    fn serialize_int<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer;
}

impl SerializeUnsignedInt for u8 {
    fn serialize_int<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(*self)
    }
}

impl SerializeUnsignedInt for u16 {
    fn serialize_int<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(*self)
    }
}

impl SerializeUnsignedInt for u32 {
    fn serialize_int<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(*self)
    }
}

impl SerializeUnsignedInt for u64 {
    fn serialize_int<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(*self)
    }
}
