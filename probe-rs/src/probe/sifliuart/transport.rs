use super::{CommandError, SifliUartCommand, SifliUartResponse, START_WORD};
use std::collections::VecDeque;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::time::{Duration, Instant};

const HEADER_LEN: usize = 4;

pub(super) struct SifliUartTransport {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: BufWriter<Box<dyn Write + Send>>,
    state: ParserState,
    console_up_buffer: VecDeque<u8>,
    response_ready: Option<Vec<u8>>,
}

enum ParserState {
    Idle,
    GotFirstByte {
        raw_frame_bytes: Vec<u8>,
    },
    ReadingHeader {
        header_buf: Vec<u8>,
        raw_frame_bytes: Vec<u8>,
    },
    ReadingPayload {
        expected_len: usize,
        payload: Vec<u8>,
        raw_frame_bytes: Vec<u8>,
    },
}

impl Default for ParserState {
    fn default() -> Self {
        Self::Idle
    }
}

impl SifliUartTransport {
    pub(super) fn new(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
            state: ParserState::Idle,
            console_up_buffer: VecDeque::new(),
            response_ready: None,
        }
    }

    pub(super) fn transaction(
        &mut self,
        command: &SifliUartCommand<'_>,
        timeout: Duration,
    ) -> Result<SifliUartResponse, CommandError> {
        self.response_ready = None;
        self.send_debug_frame(command)?;

        if matches!(command, SifliUartCommand::Exit) {
            return Ok(SifliUartResponse::Exit);
        }

        let start = Instant::now();
        loop {
            if let Some(payload) = self.response_ready.take() {
                return SifliUartResponse::from_payload(payload);
            }

            if start.elapsed() >= timeout {
                self.flush_candidate_to_console();
                return Err(CommandError::ParameterError(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Timeout",
                )));
            }

            match self.read_into_parser() {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) if should_retry_read(&error) => continue,
                Err(error) => return Err(CommandError::ProbeError(error)),
            }
        }
    }

    pub(super) fn console_read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.pump_available()?;

        let count = out.len().min(self.console_up_buffer.len());
        for (idx, byte) in self.console_up_buffer.drain(..count).enumerate() {
            out[idx] = byte;
        }
        Ok(count)
    }

    pub(super) fn console_write(&mut self, data: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(data)?;
        self.writer.flush()?;
        Ok(written)
    }

    fn pump_available(&mut self) -> io::Result<()> {
        loop {
            match self.read_into_parser() {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(error) if should_retry_read(&error) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    fn read_into_parser(&mut self) -> io::Result<bool> {
        let mut bytes = [0u8; 256];
        match self.reader.read(&mut bytes) {
            Ok(0) => Ok(false),
            Ok(count) => {
                for byte in &bytes[..count] {
                    self.feed_byte(*byte);
                }
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    fn send_debug_frame(&mut self, command: &SifliUartCommand<'_>) -> Result<(), CommandError> {
        let payload = serialize_command(command);
        let mut frame = Vec::with_capacity(START_WORD.len() + HEADER_LEN + payload.len());
        frame.extend_from_slice(&START_WORD);
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.push(0x10);
        frame.push(0x00);
        frame.extend_from_slice(&payload);

        self.writer
            .write_all(&frame)
            .map_err(CommandError::ProbeError)?;
        self.writer.flush().map_err(CommandError::ProbeError)?;
        Ok(())
    }

    fn feed_byte(&mut self, byte: u8) {
        match std::mem::take(&mut self.state) {
            ParserState::Idle => {
                if byte == START_WORD[0] {
                    self.state = ParserState::GotFirstByte {
                        raw_frame_bytes: vec![byte],
                    };
                } else {
                    self.console_up_buffer.push_back(byte);
                    self.state = ParserState::Idle;
                }
            }
            ParserState::GotFirstByte {
                mut raw_frame_bytes,
            } => {
                if byte == START_WORD[1] {
                    raw_frame_bytes.push(byte);
                    self.state = ParserState::ReadingHeader {
                        header_buf: Vec::with_capacity(HEADER_LEN),
                        raw_frame_bytes,
                    };
                } else {
                    self.console_up_buffer.extend(raw_frame_bytes);
                    self.console_up_buffer.push_back(byte);
                    self.state = ParserState::Idle;
                }
            }
            ParserState::ReadingHeader {
                mut header_buf,
                mut raw_frame_bytes,
            } => {
                header_buf.push(byte);
                raw_frame_bytes.push(byte);

                if header_buf.len() == HEADER_LEN {
                    let expected_len =
                        u16::from_le_bytes([header_buf[0], header_buf[1]]) as usize;
                    self.state = ParserState::ReadingPayload {
                        expected_len,
                        payload: Vec::with_capacity(expected_len),
                        raw_frame_bytes,
                    };
                } else {
                    self.state = ParserState::ReadingHeader {
                        header_buf,
                        raw_frame_bytes,
                    };
                }
            }
            ParserState::ReadingPayload {
                expected_len,
                mut payload,
                mut raw_frame_bytes,
            } => {
                payload.push(byte);
                raw_frame_bytes.push(byte);

                if payload.len() == expected_len {
                    if payload.last() == Some(&0x06) {
                        self.response_ready = Some(payload);
                    } else {
                        self.console_up_buffer.extend(raw_frame_bytes);
                    }
                    self.state = ParserState::Idle;
                } else {
                    self.state = ParserState::ReadingPayload {
                        expected_len,
                        payload,
                        raw_frame_bytes,
                    };
                }
            }
        }
    }

    fn flush_candidate_to_console(&mut self) {
        match std::mem::take(&mut self.state) {
            ParserState::Idle => {}
            ParserState::GotFirstByte { raw_frame_bytes }
            | ParserState::ReadingHeader {
                raw_frame_bytes, ..
            }
            | ParserState::ReadingPayload {
                raw_frame_bytes, ..
            } => {
                self.console_up_buffer.extend(raw_frame_bytes);
            }
        }
    }
}

fn serialize_command(command: &SifliUartCommand<'_>) -> Vec<u8> {
    let mut send_data = vec![];
    match command {
        SifliUartCommand::Enter => {
            send_data.extend_from_slice(&[0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x05, 0x21]);
        }
        SifliUartCommand::Exit => {
            send_data.extend_from_slice(&[0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x18, 0x21]);
        }
        SifliUartCommand::MEMRead { addr, len } => {
            send_data.push(0x40);
            send_data.push(0x72);
            send_data.extend_from_slice(&addr.to_le_bytes());
            send_data.extend_from_slice(&len.to_le_bytes());
        }
        SifliUartCommand::MEMWrite { addr, data } => {
            send_data.push(0x40);
            send_data.push(0x77);
            send_data.extend_from_slice(&addr.to_le_bytes());
            send_data.extend_from_slice(&(data.len() as u16).to_le_bytes());
            for word in *data {
                send_data.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    send_data
}

fn should_retry_read(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct SharedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedWriter {
        fn written(&self) -> Vec<u8> {
            self.bytes.lock().unwrap().clone()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ChunkReader {
        chunks: VecDeque<io::Result<Vec<u8>>>,
    }

    impl ChunkReader {
        fn from_chunks(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into_iter().map(Ok).collect(),
            }
        }
    }

    impl Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no more data"));
            };
            let chunk = chunk?;
            let len = chunk.len().min(buf.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            Ok(len)
        }
    }

    fn make_transport(reader: impl Read + Send + 'static) -> (SifliUartTransport, SharedWriter) {
        let writer = SharedWriter::default();
        let transport = SifliUartTransport::new(Box::new(reader), Box::new(writer.clone()));
        (transport, writer)
    }

    fn debug_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&START_WORD);
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.push(0x10);
        frame.push(0x00);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn pure_console_bytes_are_buffered() {
        let (mut transport, _) = make_transport(ChunkReader::from_chunks([b"hello world".to_vec()]));
        let mut out = [0u8; 32];

        let count = transport.console_read(&mut out).unwrap();

        assert_eq!(&out[..count], b"hello world");
    }

    #[test]
    fn standard_debug_frame_is_parsed() {
        let payload = [0xD1, 0x06];
        let (mut transport, writer) = make_transport(ChunkReader::from_chunks([debug_frame(&payload)]));

        let response = transport
            .transaction(&SifliUartCommand::Enter, Duration::from_millis(10))
            .unwrap();

        assert!(matches!(response, SifliUartResponse::Enter));
        assert!(!writer.written().is_empty());
        assert!(transport.console_up_buffer.is_empty());
    }

    #[test]
    fn console_and_debug_interleave_without_loss() {
        let payload = [0xD1, 0x06];
        let input = [b"log".to_vec(), debug_frame(&payload), b"more log".to_vec()].concat();
        let (mut transport, _) = make_transport(ChunkReader::from_chunks([input]));

        let response = transport
            .transaction(&SifliUartCommand::Enter, Duration::from_millis(10))
            .unwrap();
        assert!(matches!(response, SifliUartResponse::Enter));

        let mut out = [0u8; 32];
        let count = transport.console_read(&mut out).unwrap();
        assert_eq!(&out[..count], b"logmore log");
    }

    #[test]
    fn false_first_header_byte_is_restored_to_console() {
        let (mut transport, _) = make_transport(ChunkReader::from_chunks([vec![0x7E, 0x41]]));
        let mut out = [0u8; 8];

        let count = transport.console_read(&mut out).unwrap();

        assert_eq!(&out[..count], &[0x7E, 0x41]);
    }

    #[test]
    fn invalid_frame_is_reinjected_into_console() {
        let payload = [0xD1, 0x00];
        let (mut transport, _) = make_transport(ChunkReader::from_chunks([debug_frame(&payload)]));
        let mut out = [0u8; 16];

        let count = transport.console_read(&mut out).unwrap();

        assert_eq!(&out[..count], debug_frame(&payload).as_slice());
    }

    #[test]
    fn console_sequence_containing_frame_prefix_is_preserved() {
        let mut bogus = debug_frame(&[0x41, 0x42, 0x43]);
        *bogus.last_mut().unwrap() = 0x00;

        let (mut transport, _) = make_transport(ChunkReader::from_chunks([bogus.clone()]));
        let mut out = [0u8; 32];

        let count = transport.console_read(&mut out).unwrap();

        assert_eq!(&out[..count], bogus.as_slice());
    }

    #[test]
    fn transaction_timeout_restores_partial_candidate_bytes() {
        let partial = vec![0x7E, 0x79, 0x02, 0x00];
        let (mut transport, _) = make_transport(ChunkReader::from_chunks([partial.clone()]));

        let result = transport.transaction(&SifliUartCommand::Enter, Duration::from_millis(5));

        assert!(matches!(result, Err(CommandError::ParameterError(error)) if error.kind() == io::ErrorKind::TimedOut));

        let mut out = [0u8; 16];
        let count = transport.console_read(&mut out).unwrap();
        assert_eq!(&out[..count], partial.as_slice());
    }
}
