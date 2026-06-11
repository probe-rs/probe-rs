use probe_rs::Probe;

fn main() {
    let mut probe = Probe::new();
    // ... existing code ...
    probe.step_over();
    // ... existing code ...
}