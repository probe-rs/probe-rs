use std::env;
fn main() {
    let args: Vec<String> = env::args().collect();

    probe_rs_t2rust::run(&args[1], &args[2]);
}
