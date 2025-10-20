//! Support for DAP setVariable request to modify variable values at runtime
//! This module implements the setVariable request handler which allows modifying
//! variable values without pausing the debugger (pending modifications queue).
use crate::cmd::dap_server::server::debugger::DebuggerState;
use probe_rs_debug::{Variable, VariableCache, VariableValue};
use std::collections::HashMap;

/// Represents a pending variable modification
#[derive(Debug, Clone)]
pub struct PendingVariableModification {
    /// The variable key to modify
    pub variable_key: probe_rs_debug::ObjectRef,
    /// The new value as a string
    pub new_value: String,
    /// The variable name for logging
    pub variable_name: String,
}

/// Manages pending variable modifications
#[derive(Debug, Clone, Default)]
pub struct VariableModificationQueue {
    /// Queue of pending modifications indexed by variable key
    modifications: HashMap<String, PendingVariableModification>,
}

impl VariableModificationQueue {
    /// Create a new empty modification queue
    pub fn new() -> Self {
        Self {
            modifications: HashMap::new(),
        }
    }

    /// Queue a variable modification
    pub fn queue_modification(
        &mut self,
        variable_key: probe_rs_debug::ObjectRef,
        variable_name: String,
        new_value: String,
    ) {
        let key = format!("{:?}", variable_key);
        self.modifications.insert(
            key,
            PendingVariableModification {
                variable_key,
                new_value,
                variable_name,
            },
        );
    }

    /// Get the number of pending modifications
    pub fn count(&self) -> usize {
        self.modifications.len()
    }

    /// Get all pending modifications
    pub fn get_all(&self) -> Vec<PendingVariableModification> {
        self.modifications.values().cloned().collect()
    }

    /// Clear all pending modifications
    pub fn clear(&mut self) {
        self.modifications.clear();
    }

    /// Remove a specific modification
    pub fn remove(&mut self, variable_key: probe_rs_debug::ObjectRef) {
        let key = format!("{:?}", variable_key);
        self.modifications.remove(&key);
    }
}

/// Handle setVariable request from DAP client
/// This allows modifying variable values at runtime
pub fn handle_set_variable(
    debugger_state: &mut DebuggerState,
    variable_reference: i64,
    name: String,
    value: String,
) -> Result<SetVariableResponse, String> {
    // Find the variable in the cache
    let variable_key = probe_rs_debug::ObjectRef::from(variable_reference);

    // Check if debugger is running
    let is_running = debugger_state.is_running();

    if is_running {
        // Queue the modification for later application
        debugger_state.queue_variable_modification(variable_key, name.clone(), value.clone());

        Ok(SetVariableResponse {
            value: format!("Pending: {}", value),
            variable_type: Some("pending".to_string()),
            presentation_hint: None,
            indexed_variables: None,
            named_variables: None,
        })
    } else {
        // Apply the modification immediately
        match debugger_state.apply_variable_modification(variable_key, &name, &value) {
            Ok(new_value) => Ok(SetVariableResponse {
                value: new_value,
                variable_type: None,
                presentation_hint: None,
                indexed_variables: None,
                named_variables: None,
            }),
            Err(e) => Err(format!("Failed to set variable: {}", e)),
        }
    }
}

/// Response for setVariable request
#[derive(Debug, Clone)]
pub struct SetVariableResponse {
    /// The new value of the variable
    pub value: String,
    /// The type of the variable
    pub variable_type: Option<String>,
    /// Presentation hint for the variable
    pub presentation_hint: Option<String>,
    /// Number of indexed children
    pub indexed_variables: Option<i64>,
    /// Number of named children
    pub named_variables: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modification_queue() {
        let mut queue = VariableModificationQueue::new();
        assert_eq!(queue.count(), 0);

        let var_key = probe_rs_debug::ObjectRef::from(1i64);
        queue.queue_modification(var_key, "test_var".to_string(), "42".to_string());

        assert_eq!(queue.count(), 1);

        let mods = queue.get_all();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].new_value, "42");

        queue.clear();
        assert_eq!(queue.count(), 0);
    }
}
