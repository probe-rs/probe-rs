//! This is intended for internal use only.

use std::path::PathBuf;

use super::DebugInfo;

/// Get the full path to a file in the `tests` directory.
pub fn get_path_for_test_files(relative_file: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push(relative_file);
    path
}

/// Load the DebugInfo from the `elf_file` for the test.
/// `elf_file` should be the name of a file(or relative path) in the `tests` directory.
pub fn load_test_elf_as_debug_info(elf_file: &str) -> DebugInfo {
    let path = get_path_for_test_files(elf_file);
    DebugInfo::from_file(&path)
        .unwrap_or_else(|err| panic!("Failed to open file {}: {:?}", path.display(), err))
}
