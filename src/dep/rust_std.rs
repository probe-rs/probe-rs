use std::path::{self, Component, Path as StdPath, PathBuf};

use colored::Colorize;

use self::toolchain::Toolchain;

use super::rust_repo;

mod toolchain;

#[derive(Debug, Eq, PartialEq)]
pub struct Path<'p> {
    rustup_prefix: PathBuf,
    toolchain: Toolchain<'p>,
    rust_std_prefix: PathBuf,
    rust_repo_path: rust_repo::Path<'p>,
}

impl<'p> Path<'p> {
    pub fn from_std_path(path: &'p StdPath) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }

        let mut components = path.components();

        let mut rustup_prefix = PathBuf::new();
        for component in &mut components {
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
        for component in &mut components {
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

    pub fn format_short(&self) -> String {
        format!(
            "[{}]{}{}",
            self.toolchain.format_short(),
            path::MAIN_SEPARATOR,
            self.rust_repo_path.format()
        )
    }

    pub fn format_highlight(&self) -> String {
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
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn end_to_end() {
        let home = dirs::home_dir().unwrap();
        let home = home.to_str().unwrap();

        let input = PathBuf::from(home)
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

        let path = Path::from_std_path(&input).unwrap();

        let src_path = PathBuf::from("src").join("sync").join("atomic.rs");

        let expected = Path {
            rustup_prefix: PathBuf::from(home).join(".rustup").join("toolchains"),
            toolchain: Toolchain::One52(toolchain::One52 {
                channel: toolchain::Channel::Stable,
                host: "x86_64-unknown-linux-gnu",
            }),
            rust_std_prefix: PathBuf::from("lib/rustlib/src/rust"),
            rust_repo_path: rust_repo::Path::One52(rust_repo::One52Path {
                library: "library",
                crate_name: "core",
                path: &src_path,
            }),
        };

        assert_eq!(expected, path);

        let expected = PathBuf::from("[stable]")
            .join("library")
            .join("core")
            .join("src")
            .join("sync")
            .join("atomic.rs");
        let formatted_str = path.format_short();

        assert_eq!(expected.to_string_lossy(), formatted_str);
    }
}
