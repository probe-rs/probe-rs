fn main() {
    probe_rs_cli_util::meta::generate_meta();
    println!("cargo:rerun-if-changed=build.rs");
}
