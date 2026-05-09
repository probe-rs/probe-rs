use std::path::PathBuf;
use std::{env, fs};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("probe-rs.x"), include_bytes!("probe-rs.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
}
