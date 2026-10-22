//! Tests of the TAS probe against a fake TAS server on a local TCP port.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

use probe_rs::architecture::tricore::bus::{AddressMap, TriCoreBusAccess};

use super::*;

const TARGET: &str = "DAS JTAG AURDUINO 46YPEADW";
const JTAG_ID: u32 = 0x401D_A083;

/// What the fake server saw.
#[derive(Debug, Default)]
struct ServerLog {
    device_connects: Vec<u16>,
    frames: usize,
}

/// A fake TAS server with one target and a byte-addressed memory per address map.
struct FakeServer {
    port: u16,
    log: Arc<Mutex<ServerLog>>,
}

impl FakeServer {
    fn start(reject_hot_attach: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(ServerLog::default()));
        let server_log = log.clone();
        thread::spawn(move || {
            let mut memory: HashMap<(u8, u32), u8> = HashMap::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                serve(stream, &mut memory, &server_log, reject_hot_attach);
            }
        });
        Self { port, log }
    }

    fn endpoint(&self, target: Option<&str>) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            target: target.map(String::from),
        }
    }
}

fn receive(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).ok()?;
    let mut payload = vec![0; u32::from_le_bytes(length) as usize - 4];
    stream.read_exact(&mut payload).ok()?;
    Some(payload)
}

fn send(stream: &mut TcpStream, payload: &[u8]) {
    let mut message = ((payload.len() + 4) as u32).to_le_bytes().to_vec();
    message.extend_from_slice(payload);
    stream.write_all(&message).unwrap();
}

fn serve(
    mut stream: TcpStream,
    memory: &mut HashMap<(u8, u32), u8>,
    log: &Mutex<ServerLog>,
    reject_hot_attach: bool,
) {
    while let Some(rq) = receive(&mut stream) {
        let response = match rq[1] {
            0x80 => {
                let mut r = vec![0x21, 0x80, 0x00, 0x08, 0, 0, 0, 0];
                let mut name = [0u8; 64];
                name[..9].copy_from_slice(b"TasServer");
                r.extend(name);
                r.extend([2, 0, 0, 0]);
                r.resize(136, 0);
                r
            }
            0x86 => {
                let mut r = vec![0x17, 0x86, 0x00, 0x08, 0, 0, 1, 0];
                let mut t = [0u8; 88];
                t[..TARGET.len()].copy_from_slice(TARGET.as_bytes());
                t[64..68].copy_from_slice(&JTAG_ID.to_le_bytes());
                r.extend(t);
                r
            }
            0xA1 => {
                let mut r = vec![0x20, 0xA1, 0x00, 0x08, 1, 1, 0, 0];
                let mut ci = [0u8; 124];
                ci[0..4].copy_from_slice(&1060u32.to_le_bytes());
                ci[4..8].copy_from_slice(&1040u32.to_le_bytes());
                ci[8..12].copy_from_slice(&JTAG_ID.to_le_bytes());
                ci[35] = 32;
                ci[38..40].copy_from_slice(&0x8001u16.to_le_bytes());
                ci[48..48 + TARGET.len()].copy_from_slice(TARGET.as_bytes());
                r.extend(ci);
                r
            }
            0xAD => vec![0x00, 0xAD, 0x81, 0x08],
            0xA8 => {
                let option = u16::from_le_bytes([rq[4], rq[5]]);
                log.lock().unwrap().device_connects.push(option);
                if option == 0 && reject_hot_attach {
                    vec![0x00, 0xA8, 0x81, 0x46]
                } else {
                    let mut r = vec![0x02, 0xA8, 0x81, 0x08, rq[4], rq[5], 0, 0];
                    r.extend(JTAG_ID.to_le_bytes());
                    r
                }
            }
            0xB0 => {
                log.lock().unwrap().frames += 1;
                pl0(&rq, memory)
            }
            other => panic!("unexpected command {other:#04x}"),
        };
        send(&mut stream, &response);
    }
}

/// Executes a PL0 frame on the memory model.
fn pl0(rq: &[u8], memory: &mut HashMap<(u8, u32), u8>) -> Vec<u8> {
    let counter = [rq[4], rq[5]];
    let mut r = vec![0x00, 0xB0, 0x81, 0x08];
    let (mut map, mut base, mut p) = (0u8, 0u32, 8);
    loop {
        let (wl, cmd) = (rq[p], rq[p + 1]);
        let offset = u16::from_le_bytes([rq[p + 2], rq[p + 3]]) as u32;
        let address = base + offset;
        match cmd {
            0x11 => {
                map = rq[p + 2];
                p += 4;
            }
            0x15 => {
                base = offset << 16;
                p += 4;
            }
            0x20 | 0x22 | 0x24 | 0x26 | 0x2A => {
                let n = match cmd {
                    0x20 => 1,
                    0x22 => 2,
                    0x24 => 4,
                    0x26 => 8,
                    _ => 4 * if wl == 0 { 256 } else { wl as usize },
                };
                for i in 0..n {
                    memory.insert((map, address + i as u32), rq[p + 4 + i]);
                }
                p += match cmd {
                    0x20 | 0x22 | 0x24 => 8,
                    0x26 => 12,
                    _ => 4 + n,
                };
                r.extend([0x00, cmd, n.div_ceil(4) as u8, 0x80]);
            }
            0x21 | 0x23 | 0x25 | 0x27 | 0x2C => {
                let n = match cmd {
                    0x21 => 1,
                    0x23 => 2,
                    0x25 => 4,
                    0x27 => 8,
                    _ => {
                        4 * if rq[p + 4] == 0 {
                            256
                        } else {
                            rq[p + 4] as usize
                        }
                    }
                };
                p += if cmd == 0x2C { 8 } else { 4 };
                let words = n.div_ceil(4);
                r.extend([words as u8, cmd, words as u8, 0x80]);
                let mut data = (0..n)
                    .map(|i| *memory.get(&(map, address + i as u32)).unwrap_or(&0))
                    .collect::<Vec<_>>();
                data.resize(words * 4, 0);
                r.extend(data);
            }
            0xB1 => break,
            other => panic!("unexpected PL0 element {other:#04x}"),
        }
    }
    r.extend([0x00, 0xB1, counter[0], counter[1]]);
    r
}

#[test]
fn endpoints_are_parsed() {
    assert_eq!(Endpoint::parse("").unwrap(), Endpoint::default());
    assert_eq!(
        Endpoint::parse(TARGET).unwrap(),
        Endpoint {
            target: Some(TARGET.into()),
            ..Endpoint::default()
        }
    );
    assert_eq!(
        Endpoint::parse("10.0.0.2:1234/X").unwrap(),
        Endpoint {
            host: "10.0.0.2".into(),
            port: 1234,
            target: Some("X".into()),
        }
    );
    assert_eq!(
        Endpoint::parse("winbox/").unwrap(),
        Endpoint {
            host: "winbox".into(),
            ..Endpoint::default()
        }
    );
    let endpoint = Endpoint::parse("10.0.0.2:1234/X").unwrap();
    assert_eq!(Endpoint::parse(&endpoint.serial()).unwrap(), endpoint);
    assert!(Endpoint::parse("host:port/X").is_err());
}

#[test]
fn the_factory_only_opens_its_selector() {
    let other: DebugProbeSelector = "0483:3748".parse().unwrap();
    assert!(matches!(
        TasFactory.open(&other),
        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound
        ))
    ));
    let tas: DebugProbeSelector = format!("058b:0000:{TARGET}").parse().unwrap();
    assert!(TasFactory.open(&tas).is_ok());
}

#[test]
fn attach_connects_and_reads_back_memory() {
    let server = FakeServer::start(false);
    let mut probe = TasProbe::new(server.endpoint(Some(TARGET)));
    probe.attach().unwrap();
    assert_eq!(probe.target_name(), TARGET);
    assert_eq!(probe.jtag_id().unwrap(), JTAG_ID);

    // An unaligned range of more than two blocks.
    let data = (0..2500).map(|i| (i * 7) as u8).collect::<Vec<_>>();
    probe
        .write_block(AddressMap::Target, 0x7000_0003, &data)
        .unwrap();
    let mut back = vec![0; data.len()];
    probe
        .read_block(AddressMap::Target, 0x7000_0003, &mut back)
        .unwrap();
    assert_eq!(back, data);

    probe
        .write_u32(AddressMap::Target, 0xF881_FD00, 0x6)
        .unwrap();
    assert_eq!(probe.read_u32(AddressMap::Target, 0xF881_FD00).unwrap(), 6);
    // Map 15 is a separate address space.
    assert_eq!(probe.read_u32(AddressMap::Server, 0xF881_FD00).unwrap(), 0);

    probe.reset(ResetKind::ResetAndHalt).unwrap();
    assert_eq!(
        server.log.lock().unwrap().device_connects,
        vec![0x0000, 0x0003]
    );
}

#[test]
fn an_unknown_target_is_reported_with_the_available_ones() {
    let server = FakeServer::start(false);
    let mut probe = TasProbe::new(server.endpoint(Some("missing")));
    let error = probe.attach().unwrap_err().to_string();
    assert!(
        error.contains("missing") || error.contains("specific"),
        "{error}"
    );
}

#[test]
fn a_refused_hot_attach_suggests_connecting_under_reset() {
    let server = FakeServer::start(true);
    let mut probe = TasProbe::new(server.endpoint(None));
    let error = probe.attach().unwrap_err().to_string();
    assert!(error.contains("--connect-under-reset"), "{error}");

    let mut probe = TasProbe::new(server.endpoint(None));
    probe.target_reset_assert().unwrap();
    probe.attach().unwrap();
    assert_eq!(
        server.log.lock().unwrap().device_connects,
        vec![0x0000, 0x0003]
    );
}

#[test]
fn the_bus_is_only_offered_while_connected() {
    let server = FakeServer::start(false);
    let mut probe = TasProbe::new(server.endpoint(None));
    assert!(probe.try_as_tricore_bus().is_none());
    probe.attach().unwrap();
    assert!(probe.try_as_tricore_bus().is_some());
    probe.detach().unwrap();
    assert!(probe.try_as_tricore_bus().is_none());
}
