use std::collections::HashMap;

// ... existing code ...

pub struct Debugger {
    // ... existing fields ...
    macro_expansion: bool,
}

impl Debugger {
    // ... existing methods ...

    pub fn step_over(&mut self) {
        if self.macro_expansion {
            // Step over macro expansion
            self.step_over_macro();
        } else {
            // Step over normal code
            self.step_over_normal();
        }
    }

    fn step_over_macro(&mut self) {
        // Implement macro expansion stepping logic here
        // For example, you can use the `rustc` API to expand the macro
        // and then step over the expanded code
    }

    fn step_over_normal(&mut self) {
        // Implement normal stepping logic here
        // This is the existing stepping logic
    }
}

// ... existing code ...