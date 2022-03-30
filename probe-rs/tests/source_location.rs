use probe_rs::debug::{ColumnType, DebugInfo, SourceLocation};
use std::path::{Path, PathBuf};

const TEST_DATA: [(u64, u64, ColumnType); 8] = [
    // Target address, line, column
    (0x80006EA, 220, ColumnType::Column(28)),
    (0x8000770, 231, ColumnType::Column(32)),
    (0x800085C, 233, ColumnType::Column(34)),
    (0x8000958, 236, ColumnType::Column(40)),
    (0x8000942, 240, ColumnType::Column(34)),
    (0x8000A34, 272, ColumnType::Column(26)),
    (0x8000A48, 273, ColumnType::Column(23)),
    (0x8000B58, 285, ColumnType::Column(28)),
];

#[test]
fn breakpoint_location_absolute() {
    let di = DebugInfo::from_file("tests/probe-rs-debugger-test").unwrap();

    // Here we test with an absolute path, i.e. the combination of compilation directory
    // and relative path to the actual source file.
    let path = Path::new("/Users/jacknoppe/dev/probe-rs-debugger-test/src/main.rs");

    for (addr, line, col) in TEST_DATA.iter() {
        let col = if let ColumnType::Column(c) = col {
            Some(*c)
        } else {
            None
        };

        assert_eq!(
            Some(*addr),
            di.get_breakpoint_location(path, *line, col)
                .expect("Failed to find breakpoint location.")
                .first_halt_address,
            "Addresses do not match for data path={:?}, line={:?}, col={:?}",
            &path,
            line,
            col,
        );
    }
}

#[test]
fn breakpoint_location_inexact() {
    // test getting breakpoint location for an inexact location,
    // i.e. no exact entry exists for line 272 and column 1.
    let test_data = [(0x8000A34, 272, ColumnType::LeftEdge)];

    let di = DebugInfo::from_file("tests/probe-rs-debugger-test").unwrap();

    let path = Path::new("/Users/jacknoppe/dev/probe-rs-debugger-test/src/main.rs");

    for (addr, line, col) in test_data.iter() {
        let col = if let ColumnType::Column(c) = col {
            Some(*c)
        } else {
            None
        };

        assert_eq!(
            Some(*addr),
            di.get_breakpoint_location(path, *line, col)
                .expect("Failed to find valid breakpoint locations.")
                .first_halt_address,
            "Addresses do not match for data path={:?}, line={:?}, col={:?}",
            &path,
            line,
            col,
        );
    }
}

#[test]
fn source_location() {
    let di = DebugInfo::from_file("tests/probe-rs-debugger-test").unwrap();

    let file = "main.rs";

    for (addr, line, col) in TEST_DATA.iter() {
        assert_eq!(
            Some(SourceLocation {
                line: Some(*line),
                column: Some(*col),
                directory: Some(PathBuf::from(
                    "/Users/jacknoppe/dev/probe-rs-debugger-test/src"
                )),
                file: Some(file.to_owned()),
                low_pc: Some(0x80006DE),
                high_pc: Some(0x8000E0C),
            }),
            di.get_source_location(*addr)
        );
    }
}

#[test]
fn find_non_existing_unit_by_path() {
    let unit_path =
        Path::new("/Users/jacknoppe/dev/probe-rs-debugger-test/src/non-existent-path.rs");

    let debug_info = DebugInfo::from_file("tests/probe-rs-debugger-test").unwrap();

    assert!(debug_info
        .get_breakpoint_location(unit_path, 14, None)
        .is_err());
}
