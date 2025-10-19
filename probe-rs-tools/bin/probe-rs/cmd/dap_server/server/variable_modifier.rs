/// Variable modification manager for the debugger
/// Handles queuing and applying variable modifications at runtime

use std::collections::HashMap;
use probe_rs_debug::{ObjectRef, Variable, VariableCache};
use probe_rs::MemoryInterface;

/// Manages variable modifications for the debugger session
#[derive(Debug, Clone)]
pub struct VariableModifier {
    /// Queue of pending modifications
    pending_modifications: HashMap<String, PendingModification>,
    /// Successfully applied modifications
    applied_modifications: HashMap<String, AppliedModification>,
}

/// Represents a pending modification
#[derive(Debug, Clone)]
pub struct PendingModification {
    /// Variable key
    pub variable_key: ObjectRef,
    /// Variable name
    pub variable_name: String,
    /// New value as string
    pub new_value: String,
    /// Timestamp when queued
    pub queued_at: std::time::SystemTime,
}

/// Represents an applied modification
#[derive(Debug, Clone)]
pub struct AppliedModification {
    /// Variable key
    pub variable_key: ObjectRef,
    /// Variable name
    pub variable_name: String,
    /// Old value
    pub old_value: String,
    /// New value
    pub new_value: String,
    /// Timestamp when applied
    pub applied_at: std::time::SystemTime,
}

impl VariableModifier {
    /// Create a new variable modifier
    pub fn new() -> Self {
        Self {
            pending_modifications: HashMap::new(),
            applied_modifications: HashMap::new(),
        }
    }

    /// Queue a variable modification
    pub fn queue_modification(
        &mut self,
        variable_key: ObjectRef,
        variable_name: String,
        new_value: String,
    ) {
        let key = format!("{:?}", variable_key);
        self.pending_modifications.insert(
            key,
            PendingModification {
                variable_key,
                variable_name,
                new_value,
                queued_at: std::time::SystemTime::now(),
            },
        );
    }

    /// Get the number of pending modifications
    pub fn pending_count(&self) -> usize {
        self.pending_modifications.len()
    }

    /// Get all pending modifications
    pub fn get_pending(&self) -> Vec<PendingModification> {
        self.pending_modifications.values().cloned().collect()
    }

    /// Apply a pending modification
    pub fn apply_modification(
        &mut self,
        variable_key: ObjectRef,
        memory: &mut dyn MemoryInterface,
        variable_cache: &mut VariableCache,
    ) -> Result<String, String> {
        let key = format!("{:?}", variable_key);

        // Get the pending modification
        let pending = self
            .pending_modifications
            .remove(&key)
            .ok_or_else(|| format!("No pending modification for variable {:?}", variable_key))?;

        // Get the variable from cache
        let mut variable = variable_cache
            .get_variable_by_key(variable_key)
            .ok_or_else(|| format!("Variable not found in cache: {:?}", variable_key))?;

        // Store old value for history
        let old_value = variable.to_string(variable_cache);

        // Apply the modification
        variable
            .update_value(memory, variable_cache, &pending.new_value)
            .map_err(|e| format!("Failed to update variable: {}", e))?;

        // Record the applied modification
        self.applied_modifications.insert(
            key,
            AppliedModification {
                variable_key,
                variable_name: pending.variable_name.clone(),
                old_value,
                new_value: pending.new_value.clone(),
                applied_at: std::time::SystemTime::now(),
            },
        );

        Ok(pending.new_value)
    }

    /// Apply all pending modifications
    pub fn apply_all_modifications(
        &mut self,
        memory: &mut dyn MemoryInterface,
        variable_cache: &mut VariableCache,
    ) -> Result<Vec<String>, Vec<String>> {
        let mut results = Vec::new();
        let mut errors = Vec::new();

        // Get all pending modifications
        let pending: Vec<_> = self.pending_modifications.values().cloned().collect();

        for modification in pending {
            match self.apply_modification(modification.variable_key, memory, variable_cache) {
                Ok(new_value) => {
                    results.push(format!(
                        "{} = {}",
                        modification.variable_name, new_value
                    ));
                }
                Err(e) => {
                    errors.push(format!("{}: {}", modification.variable_name, e));
                }
            }
        }

        if errors.is_empty() {
            Ok(results)
        } else {
            Err(errors)
        }
    }

    /// Clear all pending modifications
    pub fn clear_pending(&mut self) {
        self.pending_modifications.clear();
    }

    /// Get modification history
    pub fn get_history(&self) -> Vec<AppliedModification> {
        self.applied_modifications.values().cloned().collect()
    }

    /// Clear modification history
    pub fn clear_history(&mut self) {
        self.applied_modifications.clear();
    }
}

impl Default for VariableModifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_modification() {
        let mut modifier = VariableModifier::new();
        let var_key = ObjectRef::from(1i64);

        modifier.queue_modification(var_key, "test_var".to_string(), "42".to_string());

        assert_eq!(modifier.pending_count(), 1);

        let pending = modifier.get_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].new_value, "42");
    }

    #[test]
    fn test_clear_pending() {
        let mut modifier = VariableModifier::new();
        let var_key = ObjectRef::from(1i64);

        modifier.queue_modification(var_key, "test_var".to_string(), "42".to_string());
        assert_eq!(modifier.pending_count(), 1);

        modifier.clear_pending();
        assert_eq!(modifier.pending_count(), 0);
    }
}
