//! Zephyr dictionary-based logging.
//!
//! Log messages refer to format strings by address; a database generated at build time turns
//! them back into text. It is embedded in the ELF file (`CONFIG_LOG_DICTIONARY_DB_EMBED`) or
//! written to `log_dictionary.json`.

mod db;
mod decoder;
mod printf;

use std::sync::Arc;

pub use db::Database;
pub use decoder::Encoding;

pub struct Decoder {
    decoder: decoder::StreamDecoder,
}

impl Decoder {
    pub fn new(db: Arc<Database>) -> Self {
        let encoding = db
            .rtt_hex()
            .map(|hex| if hex { Encoding::Hex } else { Encoding::Binary });

        Self {
            decoder: decoder::StreamDecoder::new(db, encoding),
        }
    }

    pub fn process(&mut self, data: &[u8]) -> String {
        self.decoder.process(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use test_case::test_case;

    macro_rules! fixture {
        ($dir:literal, $capture:literal) => {
            (
                include_bytes!(concat!("../../test_data/", $dir, "/log_dictionary.json"))
                    .as_slice(),
                include_bytes!(concat!("../../test_data/", $dir, "/", $capture)).as_slice(),
                include_str!(concat!("../../test_data/", $dir, "/expected.txt")),
            )
        };
    }

    type Fixture<'a> = (&'a [u8], &'a [u8], &'a str);

    fn decode(fixture: Fixture<'_>, chunk_size: usize, encoding: Option<Encoding>) -> String {
        let (db, capture, _) = fixture;
        let db = Arc::new(Database::from_bytes(db).unwrap());
        let mut decoder = decoder::StreamDecoder::new(db, encoding);

        capture
            .chunks(chunk_size)
            .map(|chunk| decoder.process(chunk))
            .collect()
    }

    // The expected output was generated from the captures with Zephyr's
    // scripts/logging/dictionary/log_parser.py.
    #[test_case(fixture!("qemu_mps2_an385_hex", "capture.hex"); "hex")]
    #[test_case(fixture!("qemu_mps2_an385_bin", "capture.bin"); "binary")]
    #[test_case(fixture!("qemu_riscv32_hex", "capture.hex"); "riscv32")]
    #[test_case(fixture!("qemu_riscv64_hex", "capture.hex"); "riscv64, 64-bit pointers")]
    fn matches_reference_parser(fixture: Fixture<'static>) {
        let expected = fixture.2;

        for chunk_size in [usize::MAX, 64, 7, 1] {
            assert_eq!(
                decode(fixture, chunk_size, None),
                expected,
                "chunk size {chunk_size}"
            );
        }
    }

    // Captures of a ztest suite that passes, fails and faults, with the database embedded into
    // the ELF file. The expected output was generated with Zephyr's log_parser.py, except that
    // non-printable characters in hexdumps are shown as '.', like Zephyr's own log output does.
    #[test_case("qemu_cortex_m3_ztest_pass"; "ztest pass")]
    #[test_case("qemu_cortex_m3_ztest_fail"; "ztest fail")]
    #[test_case("qemu_cortex_m3_ztest_fault"; "ztest fault")]
    fn ztest_captures(dir: &str) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_data")
            .join(dir);
        let elf = std::fs::read(dir.join("zephyr.elf")).unwrap();
        let capture = std::fs::read(dir.join("capture.hex")).unwrap();
        let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();

        let db = Arc::new(Database::from_elf(&elf).unwrap().unwrap());
        for chunk_size in [usize::MAX, 13, 1] {
            let mut processor = Decoder::new(db.clone());
            let decoded: String = capture
                .chunks(chunk_size)
                .map(|chunk| processor.process(chunk))
                .collect();
            assert_eq!(decoded, expected, "chunk size {chunk_size}");
        }
    }

    // Captured from an nRF52840 DK over RTT with `probe-rs run --no-timestamps
    // --target-output-file Terminal=...`, using a build without an embedded database.
    #[test_case(fixture!("nrf52840dk_ztest_rtt", "capture.hex"); "hardware rtt")]
    fn hardware_capture(fixture: Fixture<'static>) {
        for chunk_size in [usize::MAX, 64, 1] {
            assert_eq!(
                decode(fixture, chunk_size, None),
                fixture.2,
                "chunk size {chunk_size}"
            );
        }
    }

    #[test]
    fn explicit_encoding() {
        let fixture = fixture!("qemu_mps2_an385_hex", "capture.hex");
        assert_eq!(decode(fixture, 16, Some(Encoding::Hex)), fixture.2);

        let fixture = fixture!("qemu_mps2_an385_bin", "capture.bin");
        assert_eq!(decode(fixture, 16, Some(Encoding::Binary)), fixture.2);
    }

    #[test]
    fn resyncs_after_garbage() {
        let (db, capture, expected) = fixture!("qemu_mps2_an385_bin", "capture.bin");

        let mut data = vec![0xff, 0x42, b'\n'];
        data.extend_from_slice(capture);
        assert_eq!(
            decode((db, &data, expected), 5, Some(Encoding::Binary)),
            expected
        );
    }

    #[test]
    fn hex_marker_restarts_stream() {
        let (db, capture, expected) = fixture!("qemu_mps2_an385_hex", "capture.hex");

        // A partial message from before a restart must not be decoded.
        let mut data = b"00080000".to_vec();
        data.extend_from_slice(capture);
        assert_eq!(
            decode((db, &data, expected), 3, Some(Encoding::Hex)),
            expected
        );
    }

    #[test]
    fn embedded_database() {
        let elf = include_bytes!("../../test_data/qemu_mps2_an385_embedded/zephyr.elf");
        let capture = include_bytes!("../../test_data/qemu_mps2_an385_embedded/capture.hex");
        let expected = include_str!("../../test_data/qemu_mps2_an385_embedded/expected.txt");

        let db = Database::from_elf(elf).unwrap().expect("embedded database");
        let mut processor = Decoder::new(Arc::new(db));

        assert_eq!(processor.process(capture), expected);
    }

    #[test]
    fn no_embedded_database() {
        // Not an ELF file at all
        let json = include_bytes!("../../test_data/qemu_mps2_an385_hex/log_dictionary.json");
        assert!(Database::from_elf(json).unwrap().is_none());
    }

    /// The decoder must survive anything the target or a broken RTT stream can produce.
    #[test]
    fn malformed_input_does_not_panic() {
        let (db, capture, _) = fixture!("qemu_mps2_an385_bin", "capture.bin");
        let db = Arc::new(Database::from_bytes(db).unwrap());

        // Pseudo-random bytes, a deterministic sequence so failures can be reproduced.
        let mut state = 0x1234_5678_9abc_def0u64;
        let random = std::iter::repeat_with(|| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .take(8192)
        .collect::<Vec<_>>();

        for encoding in [Encoding::Binary, Encoding::Hex] {
            let mut decoder = decoder::StreamDecoder::new(db.clone(), Some(encoding));
            for chunk in random.chunks(37) {
                let _ = decoder.process(chunk);
            }
        }

        // Every truncation of a real capture, and every capture with a truncated tail.
        for len in (0..capture.len()).step_by(7) {
            let mut decoder = decoder::StreamDecoder::new(db.clone(), Some(Encoding::Binary));
            let _ = decoder.process(&capture[..len]);

            let mut decoder = decoder::StreamDecoder::new(db.clone(), Some(Encoding::Binary));
            let _ = decoder.process(&capture[len..]);
        }
    }

    /// A database from a different build resolves different strings, but must not panic.
    #[test]
    fn mismatched_database_does_not_panic() {
        let (_, capture, _) = fixture!("qemu_mps2_an385_bin", "capture.bin");
        let (other_db, ..) = fixture!("qemu_riscv64_hex", "capture.hex");

        let db = Arc::new(Database::from_bytes(other_db).unwrap());
        let mut decoder = decoder::StreamDecoder::new(db, Some(Encoding::Binary));
        let _ = decoder.process(capture);
    }

    #[test]
    fn dropped_messages() {
        let (db, _, _) = fixture!("qemu_mps2_an385_bin", "capture.bin");

        assert_eq!(
            decode((db, &[0x01, 0x05, 0x00], ""), 1, None),
            "--- 5 messages dropped ---\n"
        );
    }
}
