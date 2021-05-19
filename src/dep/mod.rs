//! Dependency path parsing

use std::{
    ffi::OsStr,
    path::{Component, Path as StdPath},
};

mod cratesio;
mod rust_repo;
mod rust_std;
mod rustc;
mod toolchain;

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

fn get_component_normal<'c>(component: Component<'c>) -> Option<&'c OsStr> {
    if let Component::Normal(string) = component {
        Some(string)
    } else {
        None
    }
}
