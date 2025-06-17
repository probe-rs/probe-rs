use serde::{Deserialize, Serialize};
use tokio_util::{bytes::BytesMut, codec::Encoder};

use super::DapCodec;

impl<T: Serialize + for<'a> Deserialize<'a> + PartialEq> Encoder<T> for DapCodec<T> {
    type Error = std::io::Error;

    fn encode(&mut self, item: T, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let response_body = serde_json::to_string(&item)?;

        let response_header = format!("Content-Length: {}\r\n\r\n", response_body.len());

        dst.extend_from_slice(response_header.as_bytes());
        dst.extend_from_slice(response_body.as_bytes());

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tokio_util::bytes::BytesMut;
    use tokio_util::codec::Encoder;

    use super::DapCodec;

    #[test]
    fn encode_frame() {
        let mut codec = DapCodec::new();
        let mut buf = BytesMut::new();

        codec
            .encode(json!({"frame": 3, "content": 6}), &mut buf)
            .unwrap();
        assert_eq!(
            &*buf,
            b"Content-Length: 23\r\n\r\n{\"content\":6,\"frame\":3}"
        )
    }
}
