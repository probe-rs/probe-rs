//! Maps the file names of a semihosting target to the resources of the host.
//!
//! The target is not trusted. The text that it sends must not select a scheme, or leave the
//! directory of a mapping.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use probe_rs_rpc::semihosting_options::{Mapping, SemihostingOptions};
use regex::Regex;

/// The resource that a file name of the target maps to.
#[derive(Debug, PartialEq, Eq)]
pub enum HostTarget {
    /// A file that the user named in an exact mapping.
    File(String),
    /// A file at `path`, which must stay beneath the directory `root`.
    Beneath { root: PathBuf, path: String },
    /// A TCP stream to an address.
    Tcp(String),
    /// A UNIX domain socket at a path.
    Unix(String),
}

#[derive(Debug)]
enum CompiledMapping {
    Exact {
        from: String,
        to: String,
    },
    Prefix {
        prefix: String,
        root: String,
    },
    Regex {
        regex: Regex,
        root: String,
        /// The text of the mapping after `root`.
        rest: String,
    },
    /// A regex mapping that uses no capture group. The user wrote all of the target.
    RegexExact {
        regex: Regex,
        to: String,
    },
}

/// The semihosting mappings, parsed and checked.
#[derive(Debug, Default)]
pub struct Mappings(Vec<CompiledMapping>);

impl Mappings {
    pub fn new(options: &SemihostingOptions) -> anyhow::Result<Self> {
        options
            .mappings()
            .iter()
            .map(CompiledMapping::new)
            .collect::<anyhow::Result<_>>()
            .map(Self)
    }

    /// Fails if the directory of a prefix or regex mapping does not exist.
    pub fn check_roots(&self) -> anyhow::Result<()> {
        for mapping in &self.0 {
            if let CompiledMapping::Prefix { root, .. } | CompiledMapping::Regex { root, .. } =
                mapping
            {
                let is_dir = Path::new(root)
                    .canonicalize()
                    .is_ok_and(|root| root.is_dir());
                if !is_dir {
                    bail!("The semihosting directory {root} does not exist.");
                }
            }
        }
        Ok(())
    }

    /// Returns the resource of the first mapping that matches `value`.
    pub fn resolve(&self, value: &str) -> Option<HostTarget> {
        for mapping in &self.0 {
            match mapping {
                CompiledMapping::Exact { from, to } => {
                    if value == from {
                        return Some(parse_exact_target(to));
                    }
                }
                CompiledMapping::Prefix { prefix, root } => {
                    if let Some(rest) = value.strip_prefix(prefix.as_str()) {
                        return is_safe_target_text(rest).then(|| HostTarget::Beneath {
                            root: root.into(),
                            path: rest.to_owned(),
                        });
                    }
                }
                CompiledMapping::Regex { regex, root, rest } => {
                    if let Some(captures) = regex.captures(value) {
                        // Group 0 is the whole file name, and `CompiledMapping::new` refuses it.
                        let safe =
                            captures.iter().skip(1).flatten().all(|group| {
                                group.is_empty() || is_safe_target_text(group.as_str())
                            });
                        if !safe {
                            return None;
                        }

                        let mut path = String::new();
                        captures.expand(rest, &mut path);
                        // Captures that are safe alone can join to an unsafe name, for example
                        // `$1.$2` with two empty captures.
                        if !path.split(['/', '\\']).all(is_safe_component) {
                            return None;
                        }
                        return Some(HostTarget::Beneath {
                            root: root.into(),
                            path,
                        });
                    }
                }
                CompiledMapping::RegexExact { regex, to } => {
                    if regex.is_match(value) {
                        return Some(parse_exact_target(to));
                    }
                }
            }
        }

        None
    }
}

impl CompiledMapping {
    fn new(mapping: &Mapping) -> anyhow::Result<Self> {
        Ok(match mapping {
            Mapping::Exact(from, to) => Self::Exact {
                from: from.clone(),
                to: to.clone(),
            },
            Mapping::Prefix(prefix, to) => {
                let root = directory_target(to, prefix)?;
                if !root.ends_with(['/', '\\']) {
                    bail!(
                        "The semihosting mapping of {prefix} must name a directory: \
                        {to} must end with a slash."
                    );
                }
                Self::Prefix {
                    prefix: prefix.clone(),
                    root: root.to_owned(),
                }
            }
            Mapping::Regex(regex, to) => {
                let compiled = Regex::new(regex)
                    .with_context(|| format!("Invalid semihosting mapping {regex}"))?;

                let references = capture_references(to);
                let Some(&(first_reference, _)) = references.first() else {
                    return Ok(Self::RegexExact {
                        regex: compiled,
                        to: unescape(to),
                    });
                };
                if references.iter().any(|(_, name)| name.parse() == Ok(0)) {
                    bail!(
                        "The semihosting mapping of {regex} uses the whole file name: \
                        use a capture group."
                    );
                }

                let target = directory_target(to, regex)?;
                // `directory_target` removes text only from the start.
                let first_reference = first_reference - (to.len() - target.len());
                let Some(separator) = target[..first_reference].rfind(['/', '\\']) else {
                    bail!(
                        "The semihosting mapping of {regex} must start with a directory, \
                        and not with {to}."
                    );
                };
                let (root, rest) = target.split_at(separator + 1);

                Self::Regex {
                    regex: compiled,
                    root: unescape(root),
                    rest: rest.to_owned(),
                }
            }
        })
    }
}

/// Removes the file scheme from a mapping that expands text from the target. The target must not
/// choose where a stream connects to, so a stream needs an exact mapping.
fn directory_target<'a>(to: &'a str, from: &str) -> anyhow::Result<&'a str> {
    if to.starts_with("tcp:") || to.starts_with("unix:") {
        bail!("The semihosting mapping of {from} to {to} must be an exact mapping.");
    }
    Ok(to.strip_prefix("file:").unwrap_or(to))
}

/// Returns the start and the name of each capture group reference in the target of a regex
/// mapping. It reads `$name`, `${name}` and `$$` the same way as `Captures::expand`, so `$00` is
/// group 0 too.
fn capture_references(to: &str) -> Vec<(usize, &str)> {
    let mut references = Vec::new();
    let mut next = 0;
    while let Some(offset) = to[next..].find('$') {
        let start = next + offset;
        let after = &to[start + 1..];
        if after.starts_with('$') {
            next = start + 2;
        } else if let Some(braced) = after.strip_prefix('{')
            && let Some(end) = braced.find('}')
        {
            references.push((start, &braced[..end]));
            next = start + 2 + end + 1;
        } else {
            let len = after
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            if len > 0 {
                references.push((start, &after[..len]));
            }
            next = start + 1 + len;
        }
    }
    references
}

/// Replaces each `$$` with `$`, as `Captures::expand` does in text with no capture group reference.
fn unescape(text: &str) -> String {
    text.replace("$$", "$")
}

fn parse_exact_target(to: &str) -> HostTarget {
    if let Some(address) = to.strip_prefix("tcp:") {
        HostTarget::Tcp(address.to_owned())
    } else if let Some(path) = to.strip_prefix("unix:") {
        HostTarget::Unix(path.to_owned())
    } else {
        HostTarget::File(to.strip_prefix("file:").unwrap_or(to).to_owned())
    }
}

/// Returns `true` if text from the target can be a part of a path beneath a directory.
fn is_safe_target_text(text: &str) -> bool {
    !text.contains(['\\', ':', '\0']) && text.split('/').all(is_safe_component)
}

fn is_safe_component(component: &str) -> bool {
    !matches!(component, "" | "." | "..") && !(cfg!(windows) && is_special_on_windows(component))
}

/// Windows opens a device for these names, also with an extension. It also removes a dot or a
/// space from the end of a name, so that a name can match another file.
fn is_special_on_windows(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or_default();
    let base = base.trim_end_matches(' ').to_uppercase();
    let is_device = match base.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let number = base
                .strip_prefix("COM")
                .or_else(|| base.strip_prefix("LPT"));
            number.is_some_and(|number| {
                matches!(
                    number,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        }
    };

    is_device || component.ends_with(['.', ' '])
}

#[cfg(test)]
mod test {
    use super::*;

    fn mappings(args: &[Mapping]) -> Mappings {
        let options: SemihostingOptions =
            serde_json::from_value(serde_json::json!({ "mappings": args })).unwrap();
        Mappings::new(&options).unwrap()
    }

    fn is_refused(mapping: Mapping) -> bool {
        let options: SemihostingOptions =
            serde_json::from_value(serde_json::json!({ "mappings": [mapping] })).unwrap();
        Mappings::new(&options).is_err()
    }

    fn prefix(from: &str, to: &str) -> Mapping {
        Mapping::Prefix(from.to_owned(), to.to_owned())
    }

    fn regex(from: &str, to: &str) -> Mapping {
        Mapping::Regex(from.to_owned(), to.to_owned())
    }

    fn exact(from: &str, to: &str) -> Mapping {
        Mapping::Exact(from.to_owned(), to.to_owned())
    }

    fn beneath(root: &str, path: &str) -> Option<HostTarget> {
        Some(HostTarget::Beneath {
            root: root.into(),
            path: path.to_owned(),
        })
    }

    #[test]
    fn an_unmapped_name_has_no_target() {
        let mappings = mappings(&[exact("a", "b"), prefix("in/", "data/")]);

        assert_eq!(mappings.resolve("c"), None);
        assert_eq!(mappings.resolve("out/a"), None);
    }

    #[test]
    fn exact_mappings_choose_the_scheme() {
        let mappings = mappings(&[
            exact("plain", "file.txt"),
            exact("file", "file:file.txt"),
            exact("tcp", "tcp:host:1234"),
            exact("unix", "unix:/run/socket"),
        ]);

        assert_eq!(
            mappings.resolve("plain"),
            Some(HostTarget::File("file.txt".to_owned()))
        );
        assert_eq!(
            mappings.resolve("file"),
            Some(HostTarget::File("file.txt".to_owned()))
        );
        assert_eq!(
            mappings.resolve("tcp"),
            Some(HostTarget::Tcp("host:1234".to_owned()))
        );
        assert_eq!(
            mappings.resolve("unix"),
            Some(HostTarget::Unix("/run/socket".to_owned()))
        );
    }

    #[test]
    fn a_stream_needs_an_exact_mapping() {
        assert!(is_refused(prefix("in/", "tcp:host:")));
        assert!(is_refused(prefix("in/", "unix:/run/")));
        assert!(is_refused(regex("^(.*)$", "tcp:host:$1")));
        assert!(is_refused(regex("^x(.*)$", "unix:/run/$1")));
    }

    #[test]
    fn a_regex_mapping_without_a_capture_is_exact() {
        let mappings = mappings(&[
            regex(r"^log\d?$", "out.txt"),
            regex("^tcp$", "tcp:host:1234"),
            regex("^cost$", "price$$.txt"),
        ]);

        assert_eq!(
            mappings.resolve("log1"),
            Some(HostTarget::File("out.txt".to_owned()))
        );
        assert_eq!(
            mappings.resolve("tcp"),
            Some(HostTarget::Tcp("host:1234".to_owned()))
        );
        assert_eq!(
            mappings.resolve("cost"),
            Some(HostTarget::File("price$.txt".to_owned()))
        );
    }

    #[test]
    fn a_mapping_that_expands_text_needs_a_directory() {
        assert!(is_refused(prefix("in/", "data")));
        assert!(is_refused(prefix("in/", "")));
        assert!(is_refused(regex("^(.*)$", "$1")));
        assert!(is_refused(regex("^(.*)$", "file:$1")));
        assert!(is_refused(regex("^x$", "/data/$0")));
    }

    #[test]
    fn a_regex_mapping_cannot_use_the_whole_file_name() {
        for to in [
            "/data/$0",
            "/data/${0}",
            "/data/$00",
            "/data/${00}",
            "/data/$000",
        ] {
            assert!(is_refused(regex("^in/(.*)$", to)), "{to:?}");
        }
    }

    #[test]
    fn an_escaped_dollar_stays_in_the_root() {
        let mappings = mappings(&[regex("^in/(.*)$", "/srv/share$$/logs/$1")]);

        assert_eq!(
            mappings.resolve("in/a.txt"),
            beneath("/srv/share$/logs/", "a.txt")
        );
    }

    #[test]
    fn capture_references_are_read_like_expand() {
        assert_eq!(
            capture_references("a$1b${name}$$c$d_2$-${x"),
            [(1, "1b"), (4, "name"), (14, "d_2")]
        );
    }

    #[test]
    fn an_invalid_regex_is_an_error() {
        assert!(is_refused(regex("(", "/data/")));
    }

    #[test]
    fn prefix_mappings_stay_beneath_the_root() {
        let mappings = mappings(&[prefix("in/", "file:/data/")]);

        assert_eq!(mappings.resolve("in/a/b.txt"), beneath("/data/", "a/b.txt"));

        for name in [
            "in/",
            "in/..",
            "in/../secret",
            "in/a/../../secret",
            "in/./a",
            "in//etc/passwd",
            "in/a//b",
            "in/a\\..\\b",
            "in/C:/secret",
            "in/file.txt:stream",
            "in/a\0b",
        ] {
            assert_eq!(mappings.resolve(name), None, "{name:?}");
        }
    }

    #[test]
    fn regex_mappings_keep_the_literal_directory_as_root() {
        let mappings = mappings(&[regex(r"^/(\d).(\d)$", "/path$1/file$2.txt")]);

        assert_eq!(mappings.resolve("/1.2"), beneath("/", "path1/file2.txt"));
    }

    #[test]
    fn regex_captures_must_be_safe() {
        let mappings = mappings(&[regex("^in/(.*)$", "/data/$1")]);

        assert_eq!(mappings.resolve("in/a/b"), beneath("/data/", "a/b"));

        for name in ["in/../secret", "in//etc/passwd", "in/a\\b", "in/tcp:host"] {
            assert_eq!(mappings.resolve(name), None, "{name:?}");
        }
    }

    #[test]
    fn joined_captures_must_be_safe() {
        let mappings = mappings(&[
            regex("^in/(.*)_(.*)$", "/data/$1.$2"),
            regex("^out/(.*)_(.*)$", "/data/$1/$2"),
        ]);

        assert_eq!(mappings.resolve("in/a_txt"), beneath("/data/", "a.txt"));
        assert_eq!(mappings.resolve("in/_"), None);
        assert_eq!(mappings.resolve("out/_a"), None);
    }

    #[test]
    fn missing_roots_are_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = format!("{}/", dir.path().display());
        let missing = format!("{}/missing/", dir.path().display());

        assert!(mappings(&[prefix("in/", &root)]).check_roots().is_ok());
        assert!(mappings(&[prefix("in/", &missing)]).check_roots().is_err());
        assert!(
            mappings(&[regex("^in/(.*)$", &format!("{missing}$1"))])
                .check_roots()
                .is_err()
        );
    }

    #[test]
    fn a_refused_name_does_not_try_the_next_mapping() {
        let mappings = mappings(&[prefix("in/", "/data/"), prefix("in/../", "/other/")]);

        assert_eq!(mappings.resolve("in/../a"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_names_are_refused() {
        for name in [
            "con", "NUL", "nul.txt", "CON .txt", "conin$", "COM1", "lpt9.log", "COM¹", "name.",
            "name ",
        ] {
            assert!(!is_safe_target_text(name), "{name:?}");
        }
        for name in ["console", "COM10", "nullable.txt"] {
            assert!(is_safe_target_text(name), "{name:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn joined_captures_cannot_name_a_device() {
        let mappings = mappings(&[regex("^in/(.*)_(.*)$", "C:/data/$1$2")]);

        assert_eq!(mappings.resolve("in/CO_N"), None);
    }
}
