use std::{env, fs, path::PathBuf};

fn main() {
    // Emit our linker script (memory.x + a `.trampoline` ebreak kept first in
    // PrgCode) into OUT_DIR and add it to the linker search path, so the
    // `-Tlink.x` in .cargo/config.toml resolves to it.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("link.x"), include_bytes!("link.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=link.x");
    println!("cargo:rerun-if-changed=build.rs");
}
