//! Zephyr dictionary logging wire format, version 3: a port of
//! `scripts/logging/dictionary/dictionary_parser/log_parser_v3.py`.
//!
//! Messages carry no framing (`include/zephyr/logging/log_output_dict.h`): a packed header,
//! then a `cbprintf` package and optional hexdump data, or a dropped-message count. The
//! stream is raw, or hex encoded after a `##ZLOGV1##` marker.

use std::fmt::Write;
use std::sync::Arc;

use super::db::Database;
use super::printf::{self, Arg, ArgSource, ArgType};

const HEX_MARKER: &[u8] = b"##ZLOGV1##";

const MSG_NORMAL: u8 = 0;
const MSG_DROPPED: u8 = 1;

const LEVELS: [&str; 5] = ["none", "err", "wrn", "inf", "dbg"];

const HEX_BYTES_IN_LINE: usize = 16;

/// Sizes and alignment rules of the target's C types.
struct Layout {
    little_endian: bool,
    ptr_size: usize,
    long_size: usize,
    long_double_size: usize,
    timestamp_size: usize,
    /// Whether `cbprintf` aligns arguments in the package to their natural alignment,
    /// see `VA_STACK_ALIGN` in `cbprintf_internal.h`.
    aligned_args: bool,
}

impl Layout {
    fn new(db: &Database) -> Self {
        let word = if db.is_64bit { 8 } else { 4 };
        let aligned_args = !matches!(
            (db.arch.as_str(), db.is_64bit),
            ("arc" | "x86", false) | ("sparc" | "riscv32e", _)
        );

        Self {
            little_endian: db.little_endian,
            ptr_size: word,
            long_size: word,
            // `long double` is a plain `double` on 32-bit Arm.
            long_double_size: if db.arch == "arm" { 8 } else { 16 },
            timestamp_size: if db.timestamp_64bit { 8 } else { 4 },
            aligned_args,
        }
    }

    fn size_of(&self, ty: ArgType) -> usize {
        match ty {
            ArgType::Int | ArgType::UInt => 4,
            ArgType::Long | ArgType::ULong => self.long_size,
            ArgType::LongLong | ArgType::ULongLong | ArgType::Double => 8,
            ArgType::Ptr => self.ptr_size,
            ArgType::LongDouble => self.long_double_size,
        }
    }

    fn align_of(&self, ty: ArgType) -> usize {
        self.size_of(ty).max(self.ptr_size)
    }

    fn read(&self, data: &[u8], offset: usize, size: usize) -> Option<u64> {
        let bytes = data.get(offset..offset.checked_add(size)?)?;
        let mut buf = [0u8; 8];

        if self.little_endian {
            buf[..size].copy_from_slice(bytes);
            Some(u64::from_le_bytes(buf))
        } else {
            buf[8 - size..].copy_from_slice(bytes);
            Some(u64::from_be_bytes(buf))
        }
    }

    fn header_size(&self) -> usize {
        // type, domain/level, package_len, data_len, source, timestamp
        1 + 1 + 2 + 2 + self.ptr_size + self.timestamp_size
    }
}

/// Reads the arguments of a `cbprintf` package.
struct PackageArgs<'a> {
    layout: &'a Layout,
    db: &'a Database,
    args: &'a [u8],
    offset: usize,
    strings: &'a [(u8, String)],
}

impl PackageArgs<'_> {
    /// Resolves a string pointer, either through the database or the strings appended to the
    /// package. `arg_offset` is the offset of the pointer from the start of the arguments.
    fn string(&self, ptr: u64, arg_offset: isize) -> String {
        if let Some(s) = self.db.find_string(ptr) {
            return s;
        }

        // The index of an appended string is the position of its pointer in the package, in
        // words, including the package header and the format string pointer.
        let idx = (arg_offset + 2 * self.layout.ptr_size as isize) / 4;
        u8::try_from(idx)
            .ok()
            .and_then(|idx| self.strings.iter().find(|(i, _)| *i == idx))
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| format!("<string@0x{ptr:x}>"))
    }
}

impl ArgSource for PackageArgs<'_> {
    fn next_arg(&mut self, ty: ArgType, is_string: bool) -> Option<Arg> {
        let size = self.layout.size_of(ty);
        let align = self.layout.align_of(ty);

        if self.layout.aligned_args {
            self.offset = self.offset.next_multiple_of(align);
        }
        let arg_offset = self.offset;
        // `long double` is decoded as a `double`, which only works where they are the same.
        let raw = self.layout.read(self.args, self.offset, size.min(8))?;
        self.offset += size;
        if self.layout.aligned_args {
            self.offset = self.offset.next_multiple_of(align);
        }

        let arg = match ty {
            ArgType::Int => Arg::Signed(i64::from(raw as u32 as i32)),
            ArgType::Long if size == 4 => Arg::Signed(i64::from(raw as u32 as i32)),
            ArgType::Long | ArgType::LongLong => Arg::Signed(raw as i64),
            ArgType::Double | ArgType::LongDouble => Arg::Float(f64::from_bits(raw)),
            ArgType::Ptr if is_string => Arg::Str(self.string(raw, arg_offset as isize)),
            ArgType::UInt | ArgType::ULong | ArgType::ULongLong | ArgType::Ptr => {
                Arg::Unsigned(raw)
            }
        };

        Some(arg)
    }
}

/// Why a message could not be decoded.
#[derive(Debug)]
enum MessageError {
    /// The header is implausible, the stream is most likely out of sync.
    BadHeader,
    /// The header is plausible, but the message could not be decoded.
    Undecodable(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Binary,
    Hex,
}

pub struct StreamDecoder {
    db: Arc<Database>,
    layout: Layout,
    encoding: Option<Encoding>,
    /// Hex encoded text that has not been converted to bytes yet.
    hex_pending: Vec<u8>,
    /// Bytes that have not been decoded yet.
    pending: Vec<u8>,
}

impl StreamDecoder {
    /// If `encoding` is `None`, it is detected from the first bytes.
    pub fn new(db: Arc<Database>, encoding: Option<Encoding>) -> Self {
        Self {
            layout: Layout::new(&db),
            db,
            encoding,
            hex_pending: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub fn process(&mut self, data: &[u8]) -> String {
        let mut out = String::new();

        let encoding = match self.encoding {
            Some(encoding) => encoding,
            None => {
                let Some(&first) = data.first() else {
                    return out;
                };
                // Messages start with a message type of 0 or 1, never with a printable
                // character.
                let encoding = if first == b'#' || first.is_ascii_hexdigit() {
                    Encoding::Hex
                } else {
                    Encoding::Binary
                };
                self.encoding = Some(encoding);
                encoding
            }
        };

        match encoding {
            Encoding::Binary => self.pending.extend_from_slice(data),
            Encoding::Hex => self.unhex(data),
        }

        self.decode_pending(&mut out);
        out
    }

    fn unhex(&mut self, data: &[u8]) {
        self.hex_pending.extend_from_slice(data);

        // A marker means the target restarted, anything before it is incomplete.
        while let Some(pos) = find(&self.hex_pending, HEX_MARKER) {
            self.hex_pending.drain(..pos + HEX_MARKER.len());
            self.pending.clear();
        }

        // Keep anything that may be the start of a marker.
        let keep = (1..HEX_MARKER.len())
            .rev()
            .find(|&n| self.hex_pending.ends_with(&HEX_MARKER[..n]))
            .unwrap_or(0);
        let convert_until = self.hex_pending.len() - keep;

        let mut digits = self.hex_pending[..convert_until]
            .iter()
            .filter_map(|&c| char::from(c).to_digit(16))
            .collect::<Vec<_>>();
        let odd = if digits.len() % 2 == 1 {
            digits.pop()
        } else {
            None
        };

        let (pairs, _) = digits.as_chunks::<2>();
        self.pending
            .extend(pairs.iter().map(|[high, low]| (high << 4 | low) as u8));

        self.hex_pending.drain(..convert_until);
        if let Some(digit) = odd {
            let c = char::from_digit(digit, 16).expect("valid hex digit");
            self.hex_pending.insert(0, c as u8);
        }
    }

    fn decode_pending(&mut self, out: &mut String) {
        let mut offset = 0;
        let mut skipped = 0;

        while let Some(&msg_type) = self.pending.get(offset) {
            let result = match msg_type {
                MSG_NORMAL => self.decode_normal(offset, out),
                MSG_DROPPED => self.decode_dropped(offset, out),
                _ => Err(MessageError::BadHeader),
            };

            match result {
                // Wait for more data.
                Ok(None) => break,
                Ok(Some(len)) => offset += len,
                Err(MessageError::BadHeader) => {
                    skipped += 1;
                    offset += 1;
                }
                Err(MessageError::Undecodable(reason)) => {
                    tracing::warn!("Failed to decode Zephyr dictionary log message: {reason}");
                    offset += self.normal_msg_len(offset).unwrap_or(1);
                }
            }
        }

        if skipped > 0 {
            tracing::debug!("Skipped {skipped} bytes of unrecognized Zephyr dictionary log data");
        }

        self.pending.drain(..offset);
    }

    fn decode_dropped(
        &self,
        offset: usize,
        out: &mut String,
    ) -> Result<Option<usize>, MessageError> {
        let Some(count) = self.layout.read(&self.pending, offset + 1, 2) else {
            return Ok(None);
        };

        writeln!(out, "--- {count} messages dropped ---").unwrap();
        Ok(Some(3))
    }

    /// `None` until the header has arrived.
    fn normal_msg_len(&self, offset: usize) -> Option<usize> {
        let package_len = self.layout.read(&self.pending, offset + 2, 2)? as usize;
        let data_len = self.layout.read(&self.pending, offset + 4, 2)? as usize;

        Some(self.layout.header_size() + package_len + data_len)
    }

    fn decode_normal(
        &self,
        offset: usize,
        out: &mut String,
    ) -> Result<Option<usize>, MessageError> {
        let layout = &self.layout;
        let header_size = layout.header_size();
        let msg = &self.pending[offset..];

        if msg.len() < header_size {
            return Ok(None);
        }

        let domain_level = msg[1];
        let package_len = layout.read(msg, 2, 2).unwrap() as usize;
        let data_len = layout.read(msg, 4, 2).unwrap() as usize;
        let source_id = layout.read(msg, 6, layout.ptr_size).unwrap();
        let timestamp = layout
            .read(msg, 6 + layout.ptr_size, layout.timestamp_size)
            .unwrap();

        let (domain_id, level) = if layout.little_endian {
            (domain_level & 0x0f, domain_level >> 4)
        } else {
            (domain_level >> 4, domain_level & 0x0f)
        };

        // The package holds at least the package header and the format string pointer.
        if package_len < 2 * layout.ptr_size || level as usize >= LEVELS.len() {
            return Err(MessageError::BadHeader);
        }

        let total_len = header_size + package_len + data_len;
        if msg.len() < total_len {
            return Ok(None);
        }

        let package = &msg[header_size..header_size + package_len];
        let hexdump = &msg[header_size + package_len..total_len];

        let text = self.format_package(package)?;

        let prefix = if level == 0 {
            String::new()
        } else {
            let source = self.db.source_name(domain_id, source_id);
            format!("[{timestamp:>10}] <{}> {source}: ", LEVELS[level as usize])
        };

        out.push_str(&prefix);
        out.push_str(&text);
        if level != 0 {
            out.push('\n');
        }

        if !hexdump.is_empty() {
            write_hexdump(out, hexdump, prefix.len());
        }

        Ok(Some(total_len))
    }

    fn format_package(&self, package: &[u8]) -> Result<String, MessageError> {
        let layout = &self.layout;

        // Package header: length of the arguments in words (including the header and the
        // format string pointer), number of appended strings, and the number of read-only and
        // read-write string indexes.
        let args_end = usize::from(package[0]) * 4;
        let num_strings = usize::from(package[1]);
        let num_ro_indexes = usize::from(package[2]);
        let num_rw_indexes = usize::from(package[3]);

        let args_start = 2 * layout.ptr_size;
        let strings_start = args_end + num_ro_indexes + num_rw_indexes;
        if args_end < args_start || strings_start > package.len() {
            return Err(MessageError::Undecodable("invalid package header"));
        }

        let strings = parse_string_table(&package[strings_start..]);
        if strings.len() != num_strings {
            return Err(MessageError::Undecodable("invalid string table"));
        }

        let mut args = PackageArgs {
            layout,
            db: &self.db,
            args: &package[args_start..args_end],
            offset: 0,
            strings: &strings,
        };

        let fmt_ptr = layout
            .read(package, layout.ptr_size, layout.ptr_size)
            .unwrap();
        let fmt = args.string(fmt_ptr, -(layout.ptr_size as isize));

        Ok(printf::format(&fmt, &mut args))
    }
}

/// Parses the strings appended to a `cbprintf` package: each is its index followed by a
/// NUL-terminated string.
fn parse_string_table(mut data: &[u8]) -> Vec<(u8, String)> {
    let mut strings = Vec::new();

    while let Some((&idx, rest)) = data.split_first() {
        let Some(end) = rest.iter().position(|&b| b == 0) else {
            break;
        };
        strings.push((idx, String::from_utf8_lossy(&rest[..end]).into_owned()));
        data = &rest[end + 1..];
    }

    strings
}

fn write_hexdump(out: &mut String, data: &[u8], prefix_len: usize) {
    let indent = " ".repeat(prefix_len);

    for line in data.chunks(HEX_BYTES_IN_LINE) {
        let mut hex = String::new();
        let mut chars = String::new();

        for (i, &byte) in line.iter().enumerate() {
            write!(hex, "{byte:02x} ").unwrap();
            chars.push(if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '.'
            });

            if i + 1 == HEX_BYTES_IN_LINE / 2 {
                hex.push(' ');
                chars.push(' ');
            }
        }

        let padding = "   ".repeat(HEX_BYTES_IN_LINE - line.len());
        writeln!(out, "{indent}{hex}{padding}|{chars}").unwrap();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
