//! The Infineon Tool Access Socket (TAS) wire protocol.
//!
//! A TAS server (`TasServer`, part of the Infineon DAS installation) owns the access hardware
//! and serves its clients over TCP. The messages follow the reference client
//! [`tas_client_api`](https://github.com/Infineon/tas_client_api) (Apache-2.0):
//!
//! - PL2: every message is `[u32 LE total size, including itself][payload]`.
//! - PL1: the payload is a sequence of elements `[wl][cmd][params..]`, `(wl + 1) * 4` bytes each.
//!   Control messages (connect, session, device connect) are one element.
//! - PL0: a memory access frame is `START`, then context and transfer elements, then `END`.

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    ops::Range,
    time::Duration,
};

use probe_rs::architecture::tricore::bus::{AddressMap, BusTransfer, TransferKind, TransferStatus};

/// The default port of a TAS server.
pub const DEFAULT_PORT: u16 = 24817;

/// PL1 success code.
pub const PL1_OK: u8 = 0x08;
/// PL1 error code `DEV_RESET`: the device was reset since this client's last request. The
/// server rejects the first request of each client after a reset with it (one request, however
/// late it comes), without running it. The client that reset the device itself through
/// `DEVICE_CONNECT` does not get it.
pub const PL1_ERR_DEVICE_RESET: u8 = 0x44;
/// PL0 (per transfer) success code.
pub const PL0_OK: u8 = 0x80;

/// `SET_DEVICE_KEY` unlock option for attaching to a running device.
pub const UNLOCK_HOT_ATTACH: u16 = 0x0001;
/// `DEVICE_CONNECT` option: attach without reset.
pub const CONNECT_HOT_ATTACH: u16 = 0x0000;
/// `DEVICE_CONNECT` option: reset the device and keep the cores halted.
pub const CONNECT_RESET_AND_HALT: u16 = 0x0003;

const CMD_SERVER_CONNECT: u8 = 0x80;
const CMD_GET_TARGETS: u8 = 0x86;
const CMD_SESSION_START: u8 = 0xA1;
const CMD_PING: u8 = 0xA2;
const CMD_DEVICE_CONNECT: u8 = 0xA8;
const CMD_SET_DEVICE_KEY: u8 = 0xAD;

const PL0_START: u8 = 0xB0;
const PL0_END: u8 = 0xB1;
const PL0_ADDR_MAP: u8 = 0x11;
const PL0_BASE_ADDR32: u8 = 0x15;
const PL0_WR8: u8 = 0x20;
const PL0_RD8: u8 = 0x21;
const PL0_WR16: u8 = 0x22;
const PL0_RD16: u8 = 0x23;
const PL0_WR32: u8 = 0x24;
const PL0_RD32: u8 = 0x25;
const PL0_WR64: u8 = 0x26;
const PL0_RD64: u8 = 0x27;
const PL0_WRBLK: u8 = 0x2A;
const PL0_RDBLK: u8 = 0x2C;
const PL0_RDBLK1KB: u8 = 0x2D;

/// Errors of the TAS protocol.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum TasError {
    /// The connection to the TAS server failed: {0}
    Io(#[from] std::io::Error),
    /// The TAS server rejected {command} with error {code:#04x}.
    Rejected {
        /// The rejected command.
        command: &'static str,
        /// The PL1 error code.
        code: u8,
    },
    /// The TAS server sent an unexpected response: {0}
    Protocol(String),
    /// The transfer can not be encoded: {0}
    InvalidTransfer(String),
    /// The TAS server has no target named "{0}". Available: {1}
    TargetNotFound(String, String),
    /// The TAS server reports no targets. Is the access hardware connected?
    NoTargets,
    /// The probe is not connected to the TAS server.
    NotConnected,
}

impl probe_rs::probe::ProbeError for TasError {}

/// The server information of a `SERVER_CONNECT` response.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// The server name, e.g. `TasServer`.
    pub name: String,
    /// Major and minor version.
    pub version: (u16, u16),
}

/// One target of a `GET_TARGETS` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasTarget {
    /// The access hardware name, e.g. `DAS JTAG AURDUINO 46YPEADW`.
    pub identifier: String,
    /// The JTAG ID code of the device, 0 if unknown.
    pub device_type: u32,
    /// The unique device ID.
    pub device_id: [u32; 4],
    /// The number of clients already connected to it.
    pub num_clients: u8,
}

/// The session information of a `SESSION_START` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConInfo {
    /// Largest request message (PL2 size).
    pub max_pl2_request: u32,
    /// Largest response message (PL2 size).
    pub max_pl2_response: u32,
    /// The JTAG ID code of the device.
    pub device_type: u32,
    /// The largest number of transfers in a PL0 frame.
    pub pl0_max_num_rw: u8,
    /// The address maps the session can use, one bit per map.
    pub pl0_addr_map_mask: u16,
    /// The access hardware name.
    pub identifier: String,
}

impl Default for ConInfo {
    fn default() -> Self {
        // The values TasServer reports for the DAS access hardware.
        Self {
            max_pl2_request: 1060,
            max_pl2_response: 1040,
            device_type: 0,
            pl0_max_num_rw: 32,
            pl0_addr_map_mask: 0x8001,
            identifier: String::new(),
        }
    }
}

fn u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn c_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn put_c_string(buf: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len().saturating_sub(1));
    buf[..n].copy_from_slice(&bytes[..n]);
}

/// Parses the 124 byte connection information of `SESSION_START` and `PING` responses.
pub fn parse_con_info(ci: &[u8]) -> Result<ConInfo, TasError> {
    if ci.len() < 124 {
        return Err(TasError::Protocol(format!(
            "connection information is {} bytes, expected 124",
            ci.len()
        )));
    }
    Ok(ConInfo {
        max_pl2_request: u32_le(&ci[0..]),
        max_pl2_response: u32_le(&ci[4..]),
        device_type: u32_le(&ci[8..]),
        pl0_max_num_rw: ci[35],
        pl0_addr_map_mask: u16_le(&ci[38..]),
        identifier: c_string(&ci[48..112]),
    })
}

/// Parses a `GET_TARGETS` response payload.
pub fn parse_targets(r: &[u8]) -> Result<Vec<TasTarget>, TasError> {
    check_pl1_response(r, CMD_GET_TARGETS, "GET_TARGETS")?;
    if r.len() < 8 {
        return Err(TasError::Protocol("short GET_TARGETS response".into()));
    }
    let count = (r[6] as usize).min((r.len() - 8) / 88);
    Ok((0..count)
        .map(|i| {
            let t = &r[8 + 88 * i..];
            TasTarget {
                identifier: c_string(&t[..64]),
                device_type: u32_le(&t[64..]),
                device_id: std::array::from_fn(|w| u32_le(&t[68 + 4 * w..])),
                num_clients: t[85],
            }
        })
        .collect())
}

fn check_pl1_response(r: &[u8], cmd: u8, command: &'static str) -> Result<(), TasError> {
    if r.len() < 4 || r[1] != cmd {
        return Err(TasError::Protocol(format!(
            "expected a {command} response, got {:02X?}",
            &r[..r.len().min(8)]
        )));
    }
    if r[3] != PL1_OK {
        return Err(TasError::Rejected {
            command,
            code: r[3],
        });
    }
    Ok(())
}

/// The bytes one transfer takes in a request and in the response.
fn transfer_sizes(transfer: &BusTransfer) -> Result<(usize, usize), TasError> {
    let n = transfer.len();
    let aligned8 = transfer.address.is_multiple_of(8);
    let words = n.div_ceil(4);
    let invalid =
        || TasError::InvalidTransfer(format!("{} bytes at {:#010x}", n, transfer.address));
    match transfer.kind {
        TransferKind::Write => match n {
            1 | 2 | 4 => Ok((8, 4)),
            8 if aligned8 => Ok((12, 4)),
            n if n > 8
                && n <= 1024
                && n.is_multiple_of(4)
                && transfer.address.is_multiple_of(4) =>
            {
                Ok((4 + n, 4))
            }
            _ => Err(invalid()),
        },
        TransferKind::Read { .. } => match n {
            1 | 2 | 4 => Ok((4, 4 + 4 * words)),
            8 if aligned8 => Ok((4, 4 + 4 * words)),
            n if n > 8 && n <= 1024 && n.is_multiple_of(8) && aligned8 => Ok((8, 4 + 4 * words)),
            _ => Err(invalid()),
        },
    }
}

/// Splits `transfers` into PL0 frames: at most `max_num_rw` transfers, within the request and
/// response size limits, and one address map per frame (the server refuses a frame that mixes
/// map 15 with map 0).
pub fn plan_frames(
    transfers: &[BusTransfer],
    info: &ConInfo,
) -> Result<Vec<Range<usize>>, TasError> {
    let max_rw = match info.pl0_max_num_rw {
        0 => 32,
        n => n as usize,
    };
    let max_rq = info.max_pl2_request as usize;
    let max_rsp = info.max_pl2_response as usize;

    // Every frame starts with base address 0, like `encode_frame`. The sizes include the PL2
    // length word and the START and END elements.
    const EMPTY_FRAME: usize = 12;

    let mut frames = Vec::new();
    let mut start = 0;
    let mut rq = EMPTY_FRAME;
    let mut rsp = EMPTY_FRAME;
    let mut base = 0u32;

    for (i, transfer) in transfers.iter().enumerate() {
        let (item_rq, item_rsp) = transfer_sizes(transfer)?;
        loop {
            let count = i - start;
            let map_element = if count == 0 && transfer.map != AddressMap::Target {
                4
            } else {
                0
            };
            let base_element = if needs_base(base, transfer.address) {
                4
            } else {
                0
            };
            let fits = count < max_rw
                && (count == 0 || transfers[start].map == transfer.map)
                && rq + map_element + base_element + item_rq <= max_rq
                && rsp + item_rsp <= max_rsp;
            if fits || count == 0 {
                rq += map_element + base_element + item_rq;
                rsp += item_rsp;
                if base_element != 0 {
                    base = transfer.address & !0xFFFF;
                }
                break;
            }
            frames.push(start..i);
            start = i;
            rq = EMPTY_FRAME;
            rsp = EMPTY_FRAME;
            base = 0;
        }
    }
    if start < transfers.len() {
        frames.push(start..transfers.len());
    }
    Ok(frames)
}

/// Whether `address` is outside the 64 KiB window at `base`, so the frame needs a base address
/// element before the transfer.
fn needs_base(base: u32, address: u32) -> bool {
    address < base || address > base.saturating_add(0xFFFF)
}

/// Encodes one PL0 frame (the PL2 payload, without the length word).
pub fn encode_frame(transfers: &[BusTransfer], pl1_cnt: u16) -> Result<Vec<u8>, TasError> {
    let map = transfers
        .first()
        .map(|t| t.map)
        .unwrap_or(AddressMap::Target);
    let mask: u16 = 1 << (map as u8);

    let mut rq = vec![0x01, PL0_START, 0xFF, 0x00];
    rq.extend(pl1_cnt.to_le_bytes());
    rq.extend(mask.to_le_bytes());

    if map != AddressMap::Target {
        rq.extend([0x00, PL0_ADDR_MAP, map as u8, 0x00]);
    }

    let mut base: u32 = 0;
    for transfer in transfers {
        if transfer.map != map {
            return Err(TasError::InvalidTransfer(
                "a frame can only use one address map".into(),
            ));
        }
        transfer_sizes(transfer)?;
        let address = transfer.address;
        if needs_base(base, address) {
            base = address & !0xFFFF;
            rq.extend([0x00, PL0_BASE_ADDR32]);
            rq.extend(((base >> 16) as u16).to_le_bytes());
        }
        let offset = (address & 0xFFFF) as u16;
        let n = transfer.len();

        match transfer.kind {
            TransferKind::Write => {
                let data = &transfer.data;
                if n <= 4 {
                    let cmd = match n {
                        1 => PL0_WR8,
                        2 => PL0_WR16,
                        _ => PL0_WR32,
                    };
                    let mut word = [0u8; 4];
                    word[..n].copy_from_slice(data);
                    rq.extend([0x01, cmd]);
                    rq.extend(offset.to_le_bytes());
                    rq.extend(word);
                } else if n == 8 {
                    rq.extend([0x02, PL0_WR64]);
                    rq.extend(offset.to_le_bytes());
                    rq.extend_from_slice(data);
                } else {
                    let words = n / 4;
                    rq.extend([(words % 256) as u8, PL0_WRBLK]);
                    rq.extend(offset.to_le_bytes());
                    rq.extend_from_slice(data);
                }
            }
            TransferKind::Read { .. } => {
                if n <= 8 {
                    let cmd = match n {
                        1 => PL0_RD8,
                        2 => PL0_RD16,
                        4 => PL0_RD32,
                        _ => PL0_RD64,
                    };
                    rq.extend([0x00, cmd]);
                    rq.extend(offset.to_le_bytes());
                } else {
                    let words = n / 4;
                    rq.extend([0x01, PL0_RDBLK]);
                    rq.extend(offset.to_le_bytes());
                    rq.extend([(words % 256) as u8, 0, 0, 0]);
                }
            }
        }
    }

    rq.extend([0x00, PL0_END]);
    rq.extend((transfers.len() as u16).to_le_bytes());
    Ok(rq)
}

/// Decodes the response to a PL0 frame into the statuses and data of `transfers`.
///
/// A frame the server rejects as a whole marks every transfer failed with the PL1 error code.
pub fn decode_frame(
    rsp: &[u8],
    transfers: &mut [BusTransfer],
    pl1_cnt: u16,
) -> Result<(), TasError> {
    if rsp.len() < 8 || rsp[0] != 0x00 || rsp[1] != PL0_START {
        return Err(TasError::Protocol(format!(
            "expected a PL0 response, got {:02X?}",
            &rsp[..rsp.len().min(8)]
        )));
    }
    let frame_error = rsp[3];
    if frame_error != PL1_OK {
        for transfer in transfers.iter_mut() {
            transfer.status = Some(TransferStatus::Failed(frame_error));
        }
        return Ok(());
    }

    let mut p = 4;
    for transfer in transfers.iter_mut() {
        let element = rsp
            .get(p..p + 4)
            .ok_or_else(|| TasError::Protocol("PL0 response is too short".into()))?;
        let (wl, cmd, wl_rw, error) = (element[0], element[1], element[2], element[3]);
        p += 4;

        let n = transfer.len();
        match transfer.kind {
            TransferKind::Write => {
                let expected_words = (n.div_ceil(4) % 256) as u8;
                transfer.status = Some(if error == PL0_OK && wl_rw == expected_words {
                    TransferStatus::Ok
                } else {
                    TransferStatus::Failed(error)
                });
            }
            TransferKind::Read { .. } => {
                let data_len = if cmd == PL0_RDBLK1KB || (cmd == PL0_RDBLK && wl_rw == 0) {
                    1024
                } else {
                    wl as usize * 4
                };
                let data = rsp
                    .get(p..p + data_len)
                    .ok_or_else(|| TasError::Protocol("PL0 read data is too short".into()))?;
                p += data_len;
                if error == PL0_OK && data_len >= n {
                    transfer.data = data[..n].to_vec();
                    transfer.status = Some(TransferStatus::Ok);
                } else {
                    transfer.data.clear();
                    transfer.status = Some(TransferStatus::Failed(if error == PL0_OK {
                        // Less data than requested.
                        0xFF
                    } else {
                        error
                    }));
                }
            }
        }
    }

    match rsp.get(p..p + 4) {
        Some([0x00, PL0_END, lo, hi]) if u16::from_le_bytes([*lo, *hi]) == pl1_cnt => Ok(()),
        other => Err(TasError::Protocol(format!(
            "expected PL0 END with counter {pl1_cnt:#06x}, got {other:02X?}"
        ))),
    }
}

/// A connection to a TAS server.
#[derive(Debug)]
pub struct Connection {
    stream: TcpStream,
    /// The PL0 frame counter. The reference client starts at 0xFFC0, so the first frame is
    /// 0xFFC1.
    pl1_cnt: u16,
    /// The device resets the server reported (`DEV_RESET`).
    device_resets: u64,
}

impl Connection {
    /// Connects to the TAS server at `host:port`.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self, TasError> {
        let mut last_error = None;
        for address in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
                    return Ok(Self {
                        stream,
                        pl1_cnt: 0xFFC0,
                        device_resets: 0,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| std::io::Error::other(format!("no address for {host}")))
            .into())
    }

    fn send(&mut self, payload: &[u8]) -> Result<(), TasError> {
        let mut message = Vec::with_capacity(payload.len() + 4);
        message.extend(((payload.len() + 4) as u32).to_le_bytes());
        message.extend_from_slice(payload);
        self.stream.write_all(&message)?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Vec<u8>, TasError> {
        let mut length = [0u8; 4];
        self.stream.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if !(4..=0x1_0000).contains(&length) {
            return Err(TasError::Protocol(format!(
                "invalid message length {length}"
            )));
        }
        let mut payload = vec![0; length - 4];
        self.stream.read_exact(&mut payload)?;
        Ok(payload)
    }

    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>, TasError> {
        self.send(payload)?;
        self.receive()
    }

    /// `SERVER_CONNECT`: identifies the client.
    pub fn server_connect(&mut self, client: &str, user: &str) -> Result<ServerInfo, TasError> {
        let mut p = [0u8; 56];
        p[0] = 0x0D;
        p[1] = CMD_SERVER_CONNECT;
        put_c_string(&mut p[4..36], client);
        put_c_string(&mut p[36..52], user);
        p[52..56].copy_from_slice(&std::process::id().to_le_bytes());
        let r = self.request(&p)?;
        check_pl1_response(&r, CMD_SERVER_CONNECT, "SERVER_CONNECT")?;
        let si = r.get(8..).unwrap_or_default();
        Ok(ServerInfo {
            name: c_string(si.get(..64).unwrap_or_default()),
            version: if si.len() >= 68 {
                (u16_le(&si[64..]), u16_le(&si[66..]))
            } else {
                (0, 0)
            },
        })
    }

    /// `GET_TARGETS`: lists the access hardware the server sees.
    pub fn get_targets(&mut self) -> Result<Vec<TasTarget>, TasError> {
        let r = self.request(&[0x00, CMD_GET_TARGETS, 0x00, 0x00])?;
        parse_targets(&r)
    }

    /// `SESSION_START`: opens a read/write session on target `identifier`.
    pub fn session_start(&mut self, identifier: &str) -> Result<ConInfo, TasError> {
        let mut p = [0u8; 112];
        p[0] = 0x1B;
        p[1] = CMD_SESSION_START;
        p[3] = 1; // client type: read/write
        put_c_string(&mut p[16..80], identifier);
        let r = self.request(&p)?;
        check_pl1_response(&r, CMD_SESSION_START, "SESSION_START")?;
        parse_con_info(r.get(8..).unwrap_or_default())
    }

    /// `PING`: keeps the session alive and refreshes its information.
    pub fn ping(&mut self) -> Result<(), TasError> {
        let r = self.request(&[0x00, CMD_PING, 0x00, 0x00])?;
        match check_pl1_response(&r, CMD_PING, "PING") {
            Err(TasError::Rejected {
                code: PL1_ERR_DEVICE_RESET,
                ..
            }) => {
                self.device_resets += 1;
                let r = self.request(&[0x00, CMD_PING, 0x00, 0x00])?;
                check_pl1_response(&r, CMD_PING, "PING")
            }
            result => result,
        }
    }

    /// The number of device resets the server reported on this connection (`DEV_RESET`).
    pub fn device_resets(&self) -> u64 {
        self.device_resets
    }

    /// `SET_DEVICE_KEY` without a key, which the following `DEVICE_CONNECT` uses.
    pub fn set_device_key(&mut self, unlock_option: u16) -> Result<(), TasError> {
        let mut p = [0u8; 8];
        p[0] = 0x01;
        p[1] = CMD_SET_DEVICE_KEY;
        p[4..6].copy_from_slice(&unlock_option.to_le_bytes());
        let r = self.request(&p)?;
        check_pl1_response(&r, CMD_SET_DEVICE_KEY, "SET_DEVICE_KEY")
    }

    /// `DEVICE_CONNECT`: connects to the device, with or without a reset. On a connected
    /// session, [`CONNECT_RESET_AND_HALT`] resets the device. Returns the JTAG ID code.
    pub fn device_connect(&mut self, option: u16) -> Result<u32, TasError> {
        let mut p = [0u8; 8];
        p[0] = 0x01;
        p[1] = CMD_DEVICE_CONNECT;
        p[4..6].copy_from_slice(&option.to_le_bytes());
        let r = self.request(&p)?;
        check_pl1_response(&r, CMD_DEVICE_CONNECT, "DEVICE_CONNECT")?;
        Ok(if r.len() >= 12 { u32_le(&r[8..]) } else { 0 })
    }

    /// Runs `transfers` as PL0 frames. Stops after the first frame with a failed transfer.
    pub fn execute(
        &mut self,
        transfers: &mut [BusTransfer],
        info: &ConInfo,
    ) -> Result<(), TasError> {
        for frame in plan_frames(transfers, info)? {
            self.pl1_cnt = self.pl1_cnt.wrapping_add(1);
            let request = encode_frame(&transfers[frame.clone()], self.pl1_cnt)?;
            tracing::trace!("TAS PL0 request: {request:02X?}");
            let mut response = self.request(&request)?;
            tracing::trace!("TAS PL0 response: {response:02X?}");
            if response.get(1) == Some(&PL0_START) && response.get(3) == Some(&PL1_ERR_DEVICE_RESET)
            {
                // The server tells of a device reset and did not run the frame: send it again.
                self.device_resets += 1;
                tracing::debug!("TAS: the device was reset, sending the frame again");
                self.pl1_cnt = self.pl1_cnt.wrapping_add(1);
                let request = encode_frame(&transfers[frame.clone()], self.pl1_cnt)?;
                response = self.request(&request)?;
                tracing::trace!("TAS PL0 response: {response:02X?}");
            }
            decode_frame(&response, &mut transfers[frame.clone()], self.pl1_cnt)?;
            if transfers[frame]
                .iter()
                .any(|t| t.status != Some(TransferStatus::Ok))
            {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! The expected bytes are from a capture of the reference TAS client against TasServer
    //! (tas-sw `pkt_capture`).

    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn read32_frame_matches_the_reference_client() {
        let frame = encode_frame(
            &[BusTransfer::read(AddressMap::Target, 0x7000_0000, 4)],
            0xFFC1,
        )
        .unwrap();
        assert_eq!(
            frame,
            hex("01 B0 FF 00 C1 FF 01 00 00 15 00 70 00 25 00 00 00 B1 01 00")
        );
    }

    #[test]
    fn write32_frame_matches_the_reference_client() {
        let frame = encode_frame(
            &[BusTransfer::write_u32(
                AddressMap::Target,
                0x7000_0000,
                0x1234_5678,
            )],
            0xFFC3,
        )
        .unwrap();
        assert_eq!(
            frame,
            hex("01 B0 FF 00 C3 FF 01 00 00 15 00 70 01 24 00 00 78 56 34 12 00 B1 01 00")
        );
    }

    #[test]
    fn block_read_frame_matches_the_reference_client() {
        let frame = encode_frame(
            &[BusTransfer::read(AddressMap::Target, 0x7000_0000, 240)],
            0xFFC6,
        )
        .unwrap();
        assert_eq!(
            frame,
            hex("01 B0 FF 00 C6 FF 01 00 00 15 00 70 01 2C 00 00 3C 00 00 00 00 B1 01 00")
        );
    }

    #[test]
    fn small_reads_share_one_base_address() {
        let frame = encode_frame(
            &[
                BusTransfer::read(AddressMap::Target, 0xDEAD_BEEF, 1),
                BusTransfer::read(AddressMap::Target, 0xDEAD_BEF0, 2),
                BusTransfer::read(AddressMap::Target, 0xDEAD_BEF2, 1),
            ],
            0xFFC7,
        )
        .unwrap();
        assert_eq!(
            frame,
            hex(
                "01 B0 FF 00 C7 FF 01 00 00 15 AD DE 00 21 EF BE 00 23 F0 BE 00 21 F2 BE 00 B1 03 00"
            )
        );
    }

    #[test]
    fn server_map_frame_matches_the_reference_client() {
        // The MCDS recorder reads the server registers in map 15 (mcd_tas client log).
        let frame = encode_frame(
            &[BusTransfer::read(AddressMap::Server, 0xD000, 104)],
            0xFFC1,
        )
        .unwrap();
        assert_eq!(
            frame,
            hex("01 B0 FF 00 C1 FF 00 80 00 11 0F 00 01 2C 00 D0 1A 00 00 00 00 B1 01 00")
        );
    }

    #[test]
    fn read_response_is_decoded() {
        let mut transfers = [BusTransfer::read(AddressMap::Target, 0x7000_0000, 4)];
        decode_frame(
            &hex("00 B0 81 08 01 25 01 80 78 56 34 12 00 B1 C4 FF"),
            &mut transfers,
            0xFFC4,
        )
        .unwrap();
        assert_eq!(transfers[0].read_u32().unwrap(), 0x1234_5678);
    }

    #[test]
    fn write_response_is_decoded() {
        let mut transfers = [BusTransfer::write_u32(AddressMap::Target, 0x7000_0000, 1)];
        decode_frame(
            &hex("00 B0 81 08 00 24 01 80 00 B1 C3 FF"),
            &mut transfers,
            0xFFC3,
        )
        .unwrap();
        assert_eq!(transfers[0].status, Some(TransferStatus::Ok));
    }

    #[test]
    fn failed_reads_carry_their_error_codes() {
        let mut transfers = [
            BusTransfer::read(AddressMap::Target, 0xDEAD_BEEF, 1),
            BusTransfer::read(AddressMap::Target, 0xDEAD_BEF0, 2),
            BusTransfer::read(AddressMap::Target, 0xDEAD_BEF2, 1),
        ];
        decode_frame(
            &hex("00 B0 81 08 00 21 00 8D 00 23 00 98 00 21 00 98 00 B1 C7 FF"),
            &mut transfers,
            0xFFC7,
        )
        .unwrap();
        assert_eq!(transfers[0].status, Some(TransferStatus::Failed(0x8D)));
        assert_eq!(transfers[1].status, Some(TransferStatus::Failed(0x98)));
        assert!(transfers[0].check().is_err());
    }

    #[test]
    fn a_wrong_frame_counter_is_an_error() {
        let mut transfers = [BusTransfer::write_u32(AddressMap::Target, 0x7000_0000, 1)];
        assert!(
            decode_frame(
                &hex("00 B0 81 08 00 24 01 80 00 B1 C2 FF"),
                &mut transfers,
                0xFFC3
            )
            .is_err()
        );
    }

    #[test]
    fn frames_respect_the_limits() {
        let info = ConInfo::default();
        // 40 word reads: 32 in the first frame.
        let reads = (0..40)
            .map(|i| BusTransfer::read(AddressMap::Target, 0x7000_0000 + 4 * i, 4))
            .collect::<Vec<_>>();
        assert_eq!(plan_frames(&reads, &info).unwrap(), vec![0..32, 32..40]);

        // Two 1 KiB blocks do not fit one response.
        let blocks = [
            BusTransfer::read(AddressMap::Target, 0x7000_0000, 1024),
            BusTransfer::read(AddressMap::Target, 0x7000_0400, 1024),
        ];
        assert_eq!(plan_frames(&blocks, &info).unwrap(), vec![0..1, 1..2]);

        // Address maps are not mixed.
        let mixed = [
            BusTransfer::read(AddressMap::Target, 0x7000_0000, 4),
            BusTransfer::read(AddressMap::Server, 0x5000_0090, 8),
        ];
        assert_eq!(plan_frames(&mixed, &info).unwrap(), vec![0..1, 1..2]);
    }

    #[test]
    fn session_information_is_parsed() {
        let mut ci = vec![0u8; 124];
        ci[0..4].copy_from_slice(&1060u32.to_le_bytes());
        ci[4..8].copy_from_slice(&1040u32.to_le_bytes());
        ci[8..12].copy_from_slice(&0x401D_A083u32.to_le_bytes());
        ci[35] = 32;
        ci[38..40].copy_from_slice(&0x8001u16.to_le_bytes());
        ci[48..48 + 26].copy_from_slice(b"DAS JTAG AURDUINO 46YPEADW");
        let info = parse_con_info(&ci).unwrap();
        assert_eq!(info.device_type, 0x401D_A083);
        assert_eq!(info.pl0_max_num_rw, 32);
        assert_eq!(info.identifier, "DAS JTAG AURDUINO 46YPEADW");
    }
}
