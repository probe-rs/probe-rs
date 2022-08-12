//! Dependency path parsing

use std::{
    ffi::OsStr,
    path::{Component, Path as StdPath},
};

mod cratesio;
mod rust_repo;
mod rust_std;
mod rustc;

#[derive(Debug, Eq, PartialEq)]
pub enum Path<'p> {
    Cratesio(cratesio::Path<'p>),
    /// Path into `rust-std` component
    RustStd(rust_std::Path<'p>),
    /// "Remapped" rust-lang/rust path AKA `/rustc` path
    Rustc(rustc::Path<'p>),
    Verbatim(&'p StdPath),
}

impl<'p> Path<'p> {
    pub fn from_std_path(path: &'p StdPath) -> Self {
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

    pub fn format_short(&self) -> String {
        match self {
            Path::Cratesio(cratesio) => cratesio.format_short(),
            Path::RustStd(rust_std) => rust_std.format_short(),
            Path::Rustc(rustc) => rustc.format_short(),
            Path::Verbatim(path) => path.display().to_string(),
        }
    }

    pub fn format_highlight(&self) -> String {
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
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn from_std_path_returns_correct_variant() {
        let home = dirs::home_dir().unwrap();
        let home = home.to_str().unwrap();

        let cratesio = PathBuf::from(home)
            .join(".cargo")
            .join("registry")
            .join("src")
            .join("github.com-1ecc6299db9ec823")
            .join("cortex-m-rt-0.6.13")
            .join("src")
            .join("lib.rs");
        assert!(matches!(Path::from_std_path(&cratesio), Path::Cratesio(_)));

        let rustc = PathBuf::from(home)
            .join("rustc")
            .join("9bc8c42bb2f19e745a63f3445f1ac248fb015e53")
            .join("library")
            .join("core")
            .join("src")
            .join("panicking.rs");
        assert!(matches!(Path::from_std_path(&rustc), Path::Rustc(_)));

        let rust_std = PathBuf::from(home)
            .join(".rustup")
            .join("toolchains")
            .join("stable-x86_64-unknown-linux-gnu")
            .join("lib")
            .join("rustlib")
            .join("src")
            .join("rust")
            .join("library")
            .join("core")
            .join("src")
            .join("sync")
            .join("atomic.rs");
        assert!(matches!(Path::from_std_path(&rust_std), Path::RustStd(_)));

        let local = PathBuf::from("src").join("lib.rs");
        assert!(matches!(Path::from_std_path(&local), Path::Verbatim(_)));
    }
}
