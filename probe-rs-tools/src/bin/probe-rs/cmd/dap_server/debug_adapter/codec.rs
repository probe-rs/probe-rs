use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

pub(crate) mod decoder;

pub(crate) struct DapCodec<T: Serialize + for<'a> Deserialize<'a>> {
    length: Option<usize>,
    header_received: bool,
    _pd: PhantomData<T>,
}
