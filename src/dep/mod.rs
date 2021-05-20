//! Dependency path parsing

use std::{
    ffi::OsStr,
    path::{Component, Path as StdPath},
};

mod cratesio;
mod rust_repo;
mod rust_std;
mod rustc;

#[derive(Debug, PartialEq)]
pub(crate) enum Path<'p> {
    Cratesio(cratesio::Path<'p>),
    /// Path into `rust-std` component
    RustStd(rust_std::Path<'p>),
    /// "Remapped" rust-lang/rust path AKA `/rustc` path
    Rustc(rustc::Path<'p>),
    Verbatim(&'p StdPath),
}

impl<'p> Path<'p> {
    pub(crate) fn from_std_path(path: &'p StdPath) -> Self {
        if let Some(rust_std) = rust_std::Path::from_std_path(path) {
            Self::RustStd(rust_std)
        } else if let Some(rustc) = rustc::Path::from_std_path(path) {
            Self::Rustc(rustc)
        } else if let Some(cratesio) = cratesio::Path::from_std_path(path) {
            Self::Cratesio(cratesio)
        } else {
            Self::Verbatim(path)
        }
    }

    pub(crate) fn format_short(&self) -> String {
        match self {
            Path::Cratesio(cratesio) => cratesio.format_short(),
            Path::RustStd(rust_std) => rust_std.format_short(),
            Path::Rustc(rustc) => rustc.format_short(),
            Path::Verbatim(path) => path.display().to_string(),
        }
    }

    pub(crate) fn format_highlight(&self) -> String {
        match self {
            Path::Cratesio(cratesio) => cratesio.format_highlight(),
            Path::RustStd(rust_std) => rust_std.format_highlight(),
            Path::Rustc(rustc) => rustc.format_highlight(),
            Path::Verbatim(path) => path.display().to_string(),
        }
    }
}

fn get_component_normal(component: Component) -> Option<&OsStr> {
    if let Component::Normal(string) = component {
        Some(string)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_std_path_returns_correct_variant() {
        let cratesio = StdPath::new(
            "/home/user/.cargo/registry/src/github.com-1ecc6299db9ec823/cortex-m-rt-0.6.13/src/lib.rs",
        );
        let rustc = StdPath::new(
            "/rustc/9bc8c42bb2f19e745a63f3445f1ac248fb015e53/library/core/src/panicking.rs",
        );
        let rust_std = StdPath::new(
            "/home/user/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/sync/atomic.rs",
        );
        let local = StdPath::new("src/lib.rs");

        assert!(matches!(Path::from_std_path(cratesio), Path::Cratesio(_)));
        assert!(matches!(Path::from_std_path(rustc), Path::Rustc(_)));
        assert!(matches!(Path::from_std_path(rust_std), Path::RustStd(_)));
        assert!(matches!(Path::from_std_path(local), Path::Verbatim(_)));
    }
}
