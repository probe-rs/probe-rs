use probe_rs::debug::{ColumnType, DebugInfo, SourceLocation};
use std::path::{Path, PathBuf};

const TEST_DATA: [(u64, u64, ColumnType); 8] = [
    // Target address, line, column
    (0x920, 11, ColumnType::LeftEdge),
    (0x922, 13, ColumnType::Column(21)),
    (0x92e, 13, ColumnType::Column(11)),
    (0x938, 27, ColumnType::Column(4)),
    (0x93a, 14, ColumnType::Column(19)),
    (0x956, 20, ColumnType::Column(12)),
    (0x962, 21, ColumnType::Column(12)),
    (0x96a, 22, ColumnType::Column(12)),
];

#[test]
fn breakpoint_location_absolute() {
    let di = DebugInfo::from_file("tests/gpio_hal_blinky").unwrap();

    // Here we test with an absolute path, i.e. the combination of compilation directory
    // and relative path to the actual source file.
    let path = Path::new("/home/dominik/Coding/microbit/examples/gpio_hal_blinky.rs");

    for (addr, line, col) in TEST_DATA.iter() {
        let col = if let ColumnType::Column(c) = col {
            Some(*c)
        } else {
            None
        };

        assert_eq!(
            Some(*addr),
            di.get_breakpoint_location(&path, *line, col)
                .expect("Failed to find breakpoint location."),
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
    // i.e. no exact entry exists for line 13 and column 14.
    let test_data = [(0x92e, 13, ColumnType::Column(14))];

    let di = DebugInfo::from_file("tests/gpio_hal_blinky").unwrap();

    let path = Path::new("/home/dominik/Coding/microbit/examples/gpio_hal_blinky.rs");

    for (addr, line, col) in test_data.iter() {
        let col = if let ColumnType::Column(c) = col {
            Some(*c)
        } else {
            None
        };

        assert_eq!(
            Some(*addr),
            di.get_breakpoint_location(&path, *line, col)
                .expect("Failed to find breakpoint location."),
            "Addresses do not match for data path={:?}, line={:?}, col={:?}",
            &path,
            line,
            col,
        );
    }
}

#[test]
fn source_location() {
    let di = DebugInfo::from_file("tests/gpio_hal_blinky").unwrap();

    let file = "gpio_hal_blinky.rs";

    for (addr, line, col) in TEST_DATA.iter() {
        assert_eq!(
            Some(SourceLocation {
                line: Some(*line),
                column: Some(*col),
                directory: Some(PathBuf::from("examples")),
                file: Some(file.to_owned()),
            }),
            di.get_source_location(*addr)
        );
    }
}

#[test]
fn find_non_existing_unit_by_path() {
    let unit_path = Path::new("/home/dominik/Coding/microbit/non_existing.rs");

    let debug_info = DebugInfo::from_file("tests/gpio_hal_blinky").unwrap();

    assert!(debug_info
        .get_breakpoint_location(&unit_path, 14, None)
        .unwrap()
        .is_none());
}
