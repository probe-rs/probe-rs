//! The CMSIS-SVD files of the AURIX devices, matched to a chip name.
//!
//! The files in `probe-rs-infineon/svd/` are the Infineon "PD" SVD files as shipped with the
//! AURIX TCF debugger extension (see `svd/README.md`). The debugger uses them for its peripheral
//! view when a launch configuration names no SVD file.

use std::path::{Path, PathBuf};

/// The environment variable with more directories to look for SVD files in.
pub const SVD_DIR_ENV: &str = "PROBE_RS_SVD_DIR";

/// The SVD file of each device family, by the start of the chip name.
const FAMILIES: &[(&str, &str)] = &[
    ("TC22", "TC22XPD"),
    ("TC23", "TC23XPD"),
    ("TC26", "TC26XBPD"),
    ("TC27", "TC27XDPD"),
    ("TC29", "TC29XBPD"),
    ("TC33", "TC33XPD"),
    ("TC35", "TC35XPD"),
    ("TC36", "TC36XPD"),
    ("TC37", "TC37XPD"),
    ("TC38", "TC38XPD"),
    ("TC39", "TC39XBPD"),
    ("TC4D", "TC4DXAPD"),
];

/// The name (without extension) of the SVD file for `chip`, for example `TC27XDPD` for `TC275`.
pub fn svd_name(chip: &str) -> Option<&'static str> {
    let chip = chip.to_ascii_uppercase();
    FAMILIES
        .iter()
        .find(|(prefix, _)| chip.starts_with(prefix))
        .map(|(_, name)| *name)
}

/// The SVD files that come with this crate.
pub fn bundled_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("svd")
}

/// Finds the SVD file for `chip`: first in `dirs`, then in the directories of
/// [`SVD_DIR_ENV`], then in [`bundled_dir`]. A plain `.svd` file wins over a `.svd.gz` file in
/// the same directory.
pub fn find_svd(chip: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = svd_name(chip)?;
    let env_dirs = std::env::var_os(SVD_DIR_ENV)
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    dirs.iter()
        .cloned()
        .chain(env_dirs)
        .chain(std::iter::once(bundled_dir()))
        .flat_map(|dir| {
            [
                dir.join(format!("{name}.svd")),
                dir.join(format!("{name}.svd.gz")),
            ]
        })
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_match_their_family() {
        assert_eq!(svd_name("TC275"), Some("TC27XDPD"));
        assert_eq!(svd_name("tc277"), Some("TC27XDPD"));
        assert_eq!(svd_name("TC375"), Some("TC37XPD"));
        assert_eq!(svd_name("STM32F407"), None);
    }

    #[test]
    fn every_family_has_a_bundled_file() {
        for (_, name) in FAMILIES {
            assert!(
                bundled_dir().join(format!("{name}.svd.gz")).is_file(),
                "{name}.svd.gz is missing"
            );
        }
        assert!(
            find_svd("TC275", &[])
                .unwrap()
                .ends_with("svd/TC27XDPD.svd.gz")
        );
    }

    #[test]
    fn a_directory_given_first_wins() {
        let dir = std::env::temp_dir().join(format!("probe-rs-svd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let own = dir.join("TC27XDPD.svd");
        std::fs::write(&own, "<device/>").unwrap();
        assert_eq!(find_svd("TC275", std::slice::from_ref(&dir)), Some(own));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
