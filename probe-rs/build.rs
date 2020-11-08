use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    // Test if we have to generate built-in targets

    if env::var("CARGO_FEATURE_BUILTIN_TARGETS").is_err() {
        return;
    }

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.rs");

    probe_rs_t2rust::run("targets", &dest_path);

    let mut rustfmt = Command::new("rustfmt");

    rustfmt.arg("--emit").arg("files").arg(&dest_path);

    let fmt_result = match rustfmt.status() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("cargo:warning=Failed to format generated target file. rustfmt not found",);
            return;
        }
        Err(e) => panic!("Failed to run rustfmt: {:?}", e),
    };

    if !fmt_result.success() {
        println!("cargo:warning=Failed to format generated target file.",);
        println!(
            "cargo:warning='rustfmt --emit files {}' failed with {}",
            dest_path.display(),
            fmt_result
        );
    }
}
