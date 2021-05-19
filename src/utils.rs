use std::path::{self, Path};

pub(crate) fn shorten_paths(path: &Path) -> String {
    if let Some(dep) = Dependency::from_path(path) {
        format!(
            "[{}]{}{}",
            dep.name_version,
            path::MAIN_SEPARATOR,
            dep.path.display()
        )
    } else {
        path.display().to_string()
    }
}

struct Dependency<'p> {
    name_version: &'p str,
    path: &'p Path,
}

impl<'p> Dependency<'p> {
    // as of Rust 1.52.1 this path looks like this on Linux
    // /home/some-user/.cargo/registry/src/github.com-0123456789abcdef/crate-name-0.1.2/src/lib.rs
    // on Windows the `/home/some-user` part becomes something else
    fn from_path(path: &'p Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }

        let mut components = path.components();
        let _registry = components.find(|component| match component {
            std::path::Component::Normal(component) => *component == "registry",
            _ => false,
        })?;

        if let std::path::Component::Normal(src) = components.next()? {
            if src != "src" {
                return None;
            }
        }

        if let std::path::Component::Normal(github) = components.next()? {
            let github = github.to_str()?;
            if !github.starts_with("github.com-") {
                return None;
            }
        }

        if let std::path::Component::Normal(name_version) = components.next()? {
            let name_version = name_version.to_str()?;
            Some(Dependency {
                name_version,
                path: components.as_path(),
            })
        } else {
            None
        }
    }
}
