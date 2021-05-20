use std::path::{self, Component, Path as StdPath, PathBuf};

use colored::Colorize;

use self::toolchain::Toolchain;

use super::rust_repo;

mod toolchain;

#[derive(Debug, PartialEq)]
pub(crate) struct Path<'p> {
    rustup_prefix: PathBuf,
    toolchain: Toolchain<'p>,
    rust_std_prefix: PathBuf,
    rust_repo_path: rust_repo::Path<'p>,
}

impl<'p> Path<'p> {
    pub(crate) fn from_std_path(path: &'p StdPath) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }

        let mut components = path.components();

        let mut rustup_prefix = PathBuf::new();
        while let Some(component) = components.next() {
            rustup_prefix.push(component);

            if let Component::Normal(component) = component {
                if component == "toolchains" {
                    break;
                }
            }
        }

        let toolchain =
            Toolchain::from_str(super::get_component_normal(components.next()?)?.to_str()?);

        let mut rust_std_prefix = PathBuf::new();
        while let Some(component) = components.next() {
            rust_std_prefix.push(component);

            if let Component::Normal(component) = component {
                if component == "rust" {
                    break;
                }
            }
        }

        let rust_repo_path = rust_repo::Path::from_std_path(components.as_path());

        Some(Path {
            rustup_prefix,
            toolchain,
            rust_std_prefix,
            rust_repo_path,
        })
    }

    pub(crate) fn format_short(&self) -> String {
        format!(
            "[{}]{}{}",
            self.toolchain.format_short(),
            path::MAIN_SEPARATOR,
            self.rust_repo_path.format()
        )
    }

    pub(crate) fn format_highlight(&self) -> String {
        format!(
            "{}{sep}{}{sep}{}{sep}{}",
            self.rustup_prefix.display().to_string().dimmed(),
            self.toolchain.format_highlight(),
            self.rust_std_prefix.display().to_string().dimmed(),
            self.rust_repo_path.format_highlight(),
            sep = path::MAIN_SEPARATOR
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path as StdPath;

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn end_to_end() {
        let input = StdPath::new("/home/user/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/sync/atomic.rs");

        let path = Path::from_std_path(input).unwrap();
        let expected = Path {
            rustup_prefix: PathBuf::from("/home/user/.rustup/toolchains"),
            toolchain: Toolchain::One52(toolchain::One52 {
                channel: toolchain::Channel::Stable,
                host: "x86_64-unknown-linux-gnu",
            }),
            rust_std_prefix: PathBuf::from("lib/rustlib/src/rust"),
            rust_repo_path: rust_repo::Path::One52(rust_repo::One52Path {
                library: "library",
                crate_name: "core",
                path: StdPath::new("src/sync/atomic.rs"),
            }),
        };

        assert_eq!(expected, path);

        let expected_str = "[stable]/library/core/src/sync/atomic.rs";
        let formatted_str = path.format_short();

        assert_eq!(expected_str, formatted_str);
    }
}
