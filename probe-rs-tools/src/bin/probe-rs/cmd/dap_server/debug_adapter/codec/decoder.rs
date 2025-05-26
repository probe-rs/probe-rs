use std::{io::BufRead, marker::PhantomData};

use serde::{Deserialize, Serialize};
use tokio_util::{bytes::BytesMut, codec::Decoder};

use super::DapCodec;

#[derive(Debug)]
pub(crate) struct Frame<T: Serialize + for<'a> Deserialize<'a>> {
    pub content: T,
}

impl<T: Serialize + for<'a> Deserialize<'a>> DapCodec<T> {
    pub(crate) fn new() -> Self {
        Self {
            length: None,
            header_received: false,
            _pd: PhantomData,
        }
    }
}

impl<T: Serialize + for<'a> Deserialize<'a>> Decoder for DapCodec<T> {
    type Item = Frame<T>;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // We have not received the full header yet, so scan for more header entries.
        if !self.header_received {
            while let Some(line) = read_line(src) {
                let line = line.inspect_err(|_| {
                    // If we fail to parse the line, we need to remove the faulty line from the buffer before erroring.
                    let _ = src.split_to((&src[..]).skip_until(b'\n').unwrap_or(0));
                })?;

                if line.is_empty() {
                    // Make sure to also split off bytes at the end of the line.
                    let _ = src.split_to(line.len() + b"\r\n".len());
                    if self.length.is_some() {
                        // The header is done parsing. Content start next line.
                        self.header_received = true;
                        // We can immediately go to parsing the content.
                        break;
                    }
                    // We unfortunately got a header end without ever receiving a length.
                    // Continue scanning through lines, flushing lines until a valid header is
                    // encountered.
                } else if let Some(length) = get_content_len(&line) {
                    // We received a content length header, we keep looking for other headers.
                    self.length = Some(length);
                    // FIXME: This reservation does not account for possible additional headers.
                    src.reserve(length + b"\r\n".len());
                    // Make sure to also split off bytes at the end of the line.
                    let _ = src.split_to(line.len() + b"\r\n".len());
                } else {
                    tracing::warn!("Unknown header payload: {line}");
                    let _ = src.split_to(line.len() + b"\r\n".len());
                }
            }
        }

        if self.header_received {
            let Some(length) = self.length else {
                // We did not get a header length but we received the header end so we cannot continue.
                // FIXME: How to flush?
                return Ok(None);
            };

            // We have received the full header, so now look for the content.
            if src.len() < length {
                // Not enough content was received yet. Wait for more bytes.
                return Ok(None);
            }

            // We need to clean up the decoder state for the next frame.
            self.header_received = false;
            self.length = None;

            // Finally parse and return the frame.q
            return Ok(Some(Frame {
                content: serde_json::from_slice::<T>(&src.split_to(length))?,
            }));
        }

        Ok(None)
    }
}

fn read_line(bytes: &mut BytesMut) -> Option<Result<String, std::io::Error>> {
    let mut buf = String::new();
    match (&bytes[..]).read_line(&mut buf) {
        Ok(0) => None,
        Ok(_n) => {
            if buf.ends_with('\n') {
                buf.pop();
                if buf.ends_with('\r') {
                    buf.pop();
                }
            } else {
                // EOF terminated lines do not count as complete yet.
                return None;
            }
            Some(Ok(buf))
        }
        Err(e) => Some(Err(e)),
    }
}

/// Parses the Content-Length header.
///
/// Discards excess characters at the start of the header to recover faster (without flushing many
/// lines) from previous malformed packets.
pub(crate) fn get_content_len(header: &str) -> Option<usize> {
    let mut parts = header.trim_end().split_ascii_whitespace().rev();

    let length = parts.next()?;
    let name = parts.next()?;

    if name.ends_with("Content-Length:") {
        length.parse::<usize>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use insta::assert_debug_snapshot;
    use serde_json::Value;
    use serde_json::json;
    use tokio_util::{bytes::BytesMut, codec::Decoder};

    use super::get_content_len;

    use super::DapCodec;

    #[test]
    fn good_single_empty_frame() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string_pretty(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0)
    }

    #[test]
    fn good_split_frame() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string_pretty(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());

        // The frame is not ready yet.
        let result = decoder.decode(&mut buf);
        assert_debug_snapshot!(result);

        buf.extend_from_slice(payload.as_bytes());

        // The frame should be ready now.
        let result = decoder.decode(&mut buf);
        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0)
    }

    #[test]
    fn good_split_frame_2() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string_pretty(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(&payload.as_bytes()[..payload.len() / 2]);
        // The frame is not ready yet.
        let result = decoder.decode(&mut buf);
        assert_debug_snapshot!(result);

        buf.extend_from_slice(&payload.as_bytes()[payload.len() / 2..]);

        // The frame should be ready now.
        let result = decoder.decode(&mut buf);
        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0)
    }

    #[test]
    fn bad_frame_wrong_length() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string_pretty(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len() + 10).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 56)
    }

    #[test]
    fn bad_frame_invalid_json() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload = "{\"test:}";
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0)
    }

    #[test]
    fn bad_frame_no_utf8() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload = "";
        buf.extend_from_slice(&[5, 189, 250, 130, 4, b'\r', b'\n']);
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 2);

        // Flush the remainder of the bytes.
        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn bad_frame_no_length() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string_pretty(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);

        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0)
    }

    #[test]
    fn recover_from_bad_frame() {
        let mut decoder = DapCodec::<Value>::new();
        let mut buf = BytesMut::new();
        let payload =
            serde_json::to_string(&json!({"seq": 3, "type": "request", "command": "test"}))
                .unwrap();
        // Send frame without length and expect second frame to arrive correctly.
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());
        buf.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        buf.extend_from_slice("\r\n".as_bytes());
        buf.extend_from_slice(payload.as_bytes());

        let result = decoder.decode(&mut buf);
        assert_debug_snapshot!(result);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn parse_valid_length_header() {
        let header = "Content-Length: 234\r\n";

        assert_eq!(234, get_content_len(header).unwrap());
    }

    #[test]
    fn parse_valid_length_header_with_prepended_data() {
        let header = "asdasdContent-Length: 234\r\n";

        assert_eq!(234, get_content_len(header).unwrap());
    }

    #[test]
    fn parse_valid_length_header_with_prepended_data_2() {
        let header = "asdasd Content-Length: 234\r\n";

        assert_eq!(234, get_content_len(header).unwrap());
    }

    #[test]
    fn parse_valid_length_header_with_prepended_data_3() {
        let header = "asdasd: Content-Length: 234\r\n";

        assert_eq!(234, get_content_len(header).unwrap());
    }

    #[test]
    fn parse_invalid_length_header() {
        let header = "Content: 234\r\n";

        assert!(get_content_len(header).is_none());
    }
}
