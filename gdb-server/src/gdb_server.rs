use gdb_protocol::{
    packet::{CheckedPacket, Kind},
    parser::Parser,
    Error,
};

use std::{
    io::{prelude::*, BufReader},
    mem,
    net::{TcpListener, TcpStream, ToSocketAddrs},
};

pub const BUF_SIZE: usize = 8 * 1024;

pub struct GdbServer<R, W>
where
    R: BufRead,
    W: Write,
{
    pub reader: R,
    pub writer: W,
    parser: Parser,
}

impl GdbServer<BufReader<TcpStream>, TcpStream> {
    pub fn listen<A>(addr: A) -> Result<Self, Error>
    where
        A: ToSocketAddrs,
    {
        let listener = TcpListener::bind(addr)?;

        let (writer, _addr) = listener.accept()?;
        writer.set_nonblocking(true);
        let reader = BufReader::new(writer.try_clone()?);

        Ok(Self::new(reader, writer))
    }
}
impl<'a> GdbServer<&'a mut &'a [u8], Vec<u8>> {
    pub fn tester(input: &'a mut &'a [u8]) -> Self {
        Self::new(input, Vec::new())
    }
    pub fn response(&mut self) -> Vec<u8> {
        mem::replace(&mut self.writer, Vec::new())
    }
}
impl<R, W> GdbServer<R, W>
where
    R: BufRead,
    W: Write,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            parser: Parser::default(),
        }
    }

    pub fn next_packet(&mut self) -> Result<Option<CheckedPacket>, Error> {
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                break Ok(None);
            }

            // println!("{:?}", std::str::from_utf8(buf));
            let (read, packet) = self.parser.feed(buf)?;
            self.reader.consume(read);

            if let Some(packet) = packet {
                break Ok(match packet.kind {
                    Kind::Packet => match packet.check() {
                        Some(checked) => {
                            self.writer.write_all(&[b'+'])?;
                            Some(checked)
                        }
                        None => {
                            self.writer.write_all(&[b'-'])?;
                            continue; // Retry
                        }
                    },
                    // Protocol specifies notifications should not be checked
                    Kind::Notification => packet.check(),
                });
            }
        }
    }

    /// Sends a packet, retrying upon any failed checksum verification
    /// on the remote.
    pub fn dispatch(&mut self, packet: &CheckedPacket) -> Result<(), Error> {
        loop {
            // std::io::stdin()
            //     .bytes() 
            //     .next();
            packet.encode(&mut self.writer)?;
            self.writer.flush()?;

            // TCP guarantees the order of packets, so theoretically
            // '+' or '-' will always be sent directly after a packet
            // is received.
            let buf = self.reader.fill_buf()?;
            match buf.first() {
                Some(b'+') => {
                    self.reader.consume(1);
                    break;
                },
                Some(b'-') => {
                    self.reader.consume(1);
                    if packet.is_valid() {
                        // Well, ok, not our fault. The packet is
                        // definitely valid, let's re-try
                        continue;
                    } else {
                        // Oh... so the user actually tried to send a
                        // packet with an invalid checksum. It's very
                        // possible that they know what they're doing
                        // though, perhaps they thought they disabled
                        // the checksum verification. So let's not
                        // panic.
                        return Err(Error::InvalidChecksum);
                    }
                },
                // Never mind... Just... hope for the best?
                _ => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdb_protocol::packet::UncheckedPacket;

    #[test]
    fn it_acknowledges_valid_packets() {
        let mut input: &[u8] = b"$packet#78";
        let mut tester = GdbServer::tester(&mut input);
        assert_eq!(
            tester.next_packet().unwrap(),
            Some(CheckedPacket::from_data(Kind::Packet, b"packet".to_vec()))
        );
        assert_eq!(tester.response(), b"+");
    }
    #[test]
    fn it_acknowledges_invalid_packets() {
        let mut input: &[u8] = b"$packet#99";
        let mut tester = GdbServer::tester(&mut input);
        assert_eq!(tester.next_packet().unwrap(), None);
        assert_eq!(tester.response(), b"-");
    }
    #[test]
    fn it_ignores_garbage() {
        let mut input: &[u8] =
            b"<garbage here yada yaya> $packet#13 $packet#37 more garbage $GARBA#GE-- $packet#78";
        let mut tester = GdbServer::tester(&mut input);
        assert_eq!(
            tester.next_packet().unwrap(),
            Some(CheckedPacket::from_data(Kind::Packet, b"packet".to_vec()))
        );
        assert_eq!(tester.response(), b"---+");
    }
    #[test]
    fn it_dispatches() {
        let mut input: &[u8] = b"";
        let mut tester = GdbServer::tester(&mut input);
        tester.dispatch(&CheckedPacket::from_data(Kind::Packet, b"hOi!!".to_vec())).unwrap();
        assert_eq!(tester.response(), b"$hOi!!#62");
    }
    #[test]
    fn it_resends() {
        let mut input: &[u8] = b"-+";
        let mut tester = GdbServer::tester(&mut input);
        tester.dispatch(&CheckedPacket::from_data(Kind::Packet, b"IMBATMAN".to_vec())).unwrap();
        assert_eq!(tester.response(), b"$IMBATMAN#49$IMBATMAN#49");
    }
    #[test]
    fn it_complains_when_the_user_lies() {
        let mut input: &[u8] = b"-";
        let mut tester = GdbServer::tester(&mut input);
        let result = tester.dispatch(&CheckedPacket::assume_checked(UncheckedPacket {
            kind: Kind::Packet,
            data: b"This sentence is false. (dontthinkaboutitdontthinkaboutit)".to_vec(),
            checksum: *b"FF",
        }));
        if let Err(Error::InvalidChecksum) = result {
        } else {
            panic!("Expected error InvalidChecksum, got {:?}", result);
        }
        // It will still send once, just in case the user has disabled checksum verification
        assert_eq!(tester.response(), b"$This sentence is false. (dontthinkaboutitdontthinkaboutit)#FF".to_vec());
    }
}
