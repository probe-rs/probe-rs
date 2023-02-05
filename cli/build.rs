#[path = "../build/version_info.rs"]
mod meta;

fn main() {
    meta::generate_meta();
    println!("cargo:rerun-if-changed=build.rs");
}
