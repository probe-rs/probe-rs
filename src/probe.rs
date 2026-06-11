use std::collections::HashMap;

// ... existing code ...

pub struct Probe {
    // ... existing fields ...
    debugger: Debugger,
}

impl Probe {
    // ... existing methods ...

    pub fn step_over(&mut self) {
        self.debugger.step_over();
    }

    // ... existing methods ...
}

// ... existing code ...