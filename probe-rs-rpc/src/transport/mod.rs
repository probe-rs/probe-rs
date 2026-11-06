use std::sync::Arc;

use futures_util::future::BoxFuture;

pub mod memory;

#[cfg(all(feature = "remote", unix))]
pub mod unix;
#[cfg(feature = "remote")]
pub mod websocket;

#[cfg(feature = "remote")]
pub use framing::*;

/// The longest message that a server accepts from a client after the login. This is also the size
/// of the receive buffer of the server. A client sends large data in parts that are smaller than
/// this, for example a file upload in parts of 512 KiB, so a large firmware image is not a problem.
pub const MAX_REQUEST_LEN: usize = 1024 * 1024;

/// Closes a connection, so that the peer can tell a clean exit from a lost connection.
#[derive(Clone)]
pub struct Closer(Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>);

impl Closer {
    pub fn new(close: impl Fn() -> BoxFuture<'static, ()> + Send + Sync + 'static) -> Self {
        Self(Arc::new(close))
    }

    /// Closes the connection, and waits until the close message is sent.
    pub async fn close(&self) {
        (self.0)().await
    }
}

#[cfg(feature = "remote")]
mod framing {
    use tokio_util::bytes::{BufMut, BytesMut};

    /// Width of the little-endian `u32` length prefix that precedes every message.
    pub const LENGTH_PREFIX_LEN: usize = size_of::<u32>();

    /// The longest message that a client accepts from a server.
    pub const MAX_REPLY_LEN: usize = 256 * 1024 * 1024;

    /// The longest message that a server accepts before the login.
    pub const MAX_LOGIN_LEN: usize = 4 * 1024;

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
    pub struct Deframer {
        buffer: BytesMut,
        max_len: usize,
    }

    impl Default for Deframer {
        fn default() -> Self {
            Self::new(MAX_REPLY_LEN)
        }
    }

    impl Deframer {
        /// Creates a deframer for messages of at most `max_len` bytes.
        pub fn new(max_len: usize) -> Self {
            Self {
                buffer: BytesMut::new(),
                max_len,
            }
        }

        /// Changes the longest message that the deframer accepts.
        pub fn set_max_len(&mut self, max_len: usize) {
            self.max_len = max_len;
        }

        /// Returns `true` if the next message is too long. The peer must not get more data
        /// through, so the connection must end.
        pub fn next_is_too_long(&self) -> bool {
            self.buffer.get(..LENGTH_PREFIX_LEN).is_some_and(|prefix| {
                u32::from_le_bytes(prefix.try_into().unwrap()) as usize > self.max_len
            })
        }

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
        fn too_long_message_is_detected_from_the_prefix() {
            let mut deframer = Deframer::new(4);

            deframer.push(&frame(b"four"));
            assert!(!deframer.next_is_too_long());
            assert_eq!(deframer.next_message().as_deref(), Some(&b"four"[..]));

            deframer.push(&5u32.to_le_bytes());
            assert!(deframer.next_is_too_long());
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
