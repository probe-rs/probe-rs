pub mod memory;

#[cfg(all(feature = "remote", unix))]
pub mod unix;
#[cfg(feature = "remote")]
pub mod websocket;

#[cfg(feature = "remote")]
pub use framing::*;

#[cfg(feature = "remote")]
mod framing {
    use tokio_util::bytes::{BufMut, BytesMut};

    /// Width of the little-endian `u32` length prefix that precedes every message.
    pub const LENGTH_PREFIX_LEN: usize = size_of::<u32>();

    /// Prefix `msg` with its length, ready to be written to the wire.
    ///
    /// The caller is responsible for rejecting messages longer than [`u32::MAX`].
    pub fn frame(msg: &[u8]) -> BytesMut {
        let mut bytes = BytesMut::with_capacity(LENGTH_PREFIX_LEN + msg.len());
        bytes.put_u32_le(msg.len() as u32);
        bytes.put_slice(msg);
        bytes
    }

    /// Reassembles [`frame`]d messages from a transport that delivers arbitrarily
    /// chunked reads, which may split or coalesce messages.
    #[derive(Default)]
    pub struct Deframer {
        buffer: BytesMut,
    }

    impl Deframer {
        pub fn push(&mut self, chunk: &[u8]) {
            self.buffer.extend_from_slice(chunk);
        }

        /// Take the next complete message, if the buffer already holds one.
        pub fn next_message(&mut self) -> Option<Vec<u8>> {
            let prefix = self.buffer.get(..LENGTH_PREFIX_LEN)?;
            let len = u32::from_le_bytes(prefix.try_into().unwrap()) as usize;

            let message = self
                .buffer
                .get(LENGTH_PREFIX_LEN..LENGTH_PREFIX_LEN + len)?
                .to_vec();
            let _ = self.buffer.split_to(LENGTH_PREFIX_LEN + len);

            Some(message)
        }
    }

    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn roundtrip_split_across_chunks() {
            let mut deframer = Deframer::default();
            let framed = frame(b"hello");

            for byte in framed.iter() {
                assert_eq!(deframer.next_message(), None);
                deframer.push(&[*byte]);
            }

            assert_eq!(deframer.next_message().as_deref(), Some(&b"hello"[..]));
            assert_eq!(deframer.next_message(), None);
        }

        #[test]
        fn several_messages_in_one_chunk() {
            let mut deframer = Deframer::default();
            let mut chunk = frame(b"one");
            chunk.extend_from_slice(&frame(b"two"));
            deframer.push(&chunk);

            assert_eq!(deframer.next_message().as_deref(), Some(&b"one"[..]));
            assert_eq!(deframer.next_message().as_deref(), Some(&b"two"[..]));
            assert_eq!(deframer.next_message(), None);
        }

        #[test]
        fn empty_message() {
            let mut deframer = Deframer::default();
            deframer.push(&frame(b""));

            assert_eq!(deframer.next_message().as_deref(), Some(&b""[..]));
            assert_eq!(deframer.next_message(), None);
        }
    }
}
