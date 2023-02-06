/// Generates a `meta.rs` file in a crates `OUT_DIR`.
///
/// This is intended to be used in the `build.rs` of a crate.
///
/// # Examples
///
/// ```no_run
/// probe_rs_cli_util::meta::generate_meta();
/// println!("cargo:rerun-if-changed=build.rs");
/// ```
pub fn generate_meta() {
    const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");
    const GIT_VERSION: &str = git_version::git_version!(fallback = "crates.io");
    let long_version: String = format!("{CARGO_VERSION}\ngit commit: {GIT_VERSION}");

    let out_dir = std::env::var_os("OUT_DIR").unwrap();
    let dest_path = std::path::Path::new(&out_dir).join("meta.rs");
    std::fs::write(
        dest_path,
        format!(
            r#"#[allow(dead_code)]mod meta {{
pub const CARGO_VERSION: &str = "{CARGO_VERSION}";
pub const GIT_VERSION: &str = "{GIT_VERSION}";
pub const LONG_VERSION: &str = "{long_version}";
}}        "#
        ),
    )
    .unwrap();
}
