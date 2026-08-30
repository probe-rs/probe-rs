use gimli::DebuggingInformationEntry;
use probe_rs::MemoryInterface;

use crate::{
    DebugError, DebugInfo, GimliReader, ObjectRef, StackFrameInfo, Variable, VariableCache,
    VariableName, VariableNodeType, VariableType, VariableValue,
    language::rust::{Rust, RustPath, is_named, type_ident},
    unit_info::UnitInfo,
};

#[derive(Clone, Copy)]
enum ModulePath {
    Prefix(&'static [&'static str]),
    Exact(&'static [&'static str]),
}

#[derive(Clone, Copy)]
enum Ident {
    Exact(&'static str),
    Prefix(&'static str),
}

#[derive(Clone, Copy)]
struct TypeMatch {
    modules: ModulePath,
    ident: Ident,
}

impl TypeMatch {
    fn matches(&self, path: &RustPath) -> bool {
        let (is_prefix, mut modules) = match self.modules {
            ModulePath::Prefix(prefix) => (true, prefix),
            ModulePath::Exact(modules) => (false, modules),
        };

        // Crate matches?
        let krate = *modules
            .split_off_first()
            .expect("must have at least one path component");
        if krate == "*" {
            // any crate matches
        } else if !krate.split('|').any(|k| path.crate_name == k) {
            return false;
        }

        // Path matches?
        let modules_match = match is_prefix {
            true => path.modules_start_with(modules),
            false => path.modules_eq(modules),
        };
        if !modules_match {
            return false;
        }

        // Type name matches?
        match self.ident {
            Ident::Exact(ident) => path.ident == ident,
            Ident::Prefix(prefix) => path.ident.starts_with(prefix),
        }
    }
}

#[derive(Clone, Copy)]
enum InnerLayer {
    /// Remove this type when it is the next layer.
    Skip(TypeMatch),
    /// Remove this type when it is the next layer. If it is not, try the next pattern layer.
    SkipOpt(TypeMatch),
    /// The remaining inner type. The rewrite keeps it.
    Payload,
}

/// Match a nested type chain and drop implementation layers.
struct WrapperChain {
    outer: TypeMatch,
    inners: &'static [InnerLayer],
    /// Copy a base-type payload onto `outer`. A `VolatileCell` must not do this.
    inline_leaf: bool,
    /// Use the payload type name on `outer`.
    hide_outer: bool,
}

impl WrapperChain {
    fn skip_inner_indices(&self, inners: &[Option<RustPath>]) -> Option<Vec<usize>> {
        let mut i = 0;
        let mut skip = Vec::new();
        for layer in self.inners {
            match layer {
                InnerLayer::Skip(type_match) => {
                    let path = inners.get(i)?.as_ref()?;
                    if !type_match.matches(path) {
                        return None;
                    }
                    skip.push(i);
                    i += 1;
                }
                InnerLayer::SkipOpt(type_match) => {
                    if inners
                        .get(i)
                        .and_then(|path| path.as_ref())
                        .is_some_and(|path| type_match.matches(path))
                    {
                        skip.push(i);
                        i += 1;
                    }
                }
                InnerLayer::Payload => {
                    if inners.get(i).is_some() || !skip.is_empty() {
                        return Some(skip);
                    }
                    return None;
                }
            }
        }
        Some(skip)
    }
}

impl Rust {
    /// Rewrites known nested type chains after their member DIE is in the cache.
    #[expect(clippy::too_many_arguments)]
    fn flatten_known_wrapper(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(path) = RustPath::from_die(unit_info, debug_info, node) else {
            return Ok(());
        };
        if !WRAPPER_CHAINS.iter().any(|rule| rule.outer.matches(&path)) {
            return Ok(());
        }

        let inners = collect_single_child_chain(debug_info, variable, memory, cache, frame_info)?;
        let inner_paths: Vec<_> = inners
            .iter()
            .map(|inner| RustPath::from_variable(debug_info, inner))
            .collect();
        let Some((rule, skip)) = WRAPPER_CHAINS.iter().find_map(|rule| {
            if !rule.outer.matches(&path) {
                return None;
            }
            Some((rule, rule.skip_inner_indices(&inner_paths)?))
        }) else {
            return Ok(());
        };

        let mut skipped_value = None;
        let mut skipped_type = None;
        for idx in skip {
            let Some(skipped) = cache.get_variable_by_key(inners[idx].variable_key) else {
                return Ok(());
            };
            if !cache.has_children(&skipped)
                && skipped.value.is_valid()
                && !skipped.value.is_empty()
            {
                skipped_value = Some(skipped.value.clone());
                skipped_type = Some(skipped.type_name.clone());
            }
            let skipped_key = skipped.variable_key;
            cache.adopt_grand_children(variable, &skipped)?;
            if cache.get_variable_by_key(skipped_key).is_some() {
                // `adopt_grand_children` keeps a node that already holds an inlined base value.
                if skipped.value.is_valid() && !skipped.value.is_empty() {
                    skipped_value = Some(skipped.value.clone());
                    skipped_type = Some(skipped.type_name.clone());
                    cache.remove_cache_entry(skipped_key)?;
                } else {
                    return Ok(());
                }
            }
        }

        let children: Vec<_> = cache.get_children(variable.variable_key).cloned().collect();
        let inner = match children.as_slice() {
            [inner] => inner,
            [] => {
                if rule.hide_outer
                    && let Some(ty) = skipped_type
                {
                    variable.type_name = ty;
                }
                if (rule.inline_leaf || rule.hide_outer)
                    && let Some(value) = skipped_value
                {
                    variable.set_value(value);
                    variable.variable_node_type = VariableNodeType::DoNotRecurse;
                    cache.update_variable(variable)?;
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        if rule.hide_outer {
            if matches!(inner.type_name, VariableType::Base(_)) {
                variable.type_name = inner.type_name.clone();
            }
            if !cache.has_children(inner) && inner.value.is_valid() && !inner.value.is_empty() {
                variable.type_name = inner.type_name.clone();
                variable.set_value(inner.value.clone());
                cache.remove_cache_entry(inner.variable_key)?;
                variable.variable_node_type = VariableNodeType::DoNotRecurse;
            }
            cache.update_variable(variable)?;
            return Ok(());
        }

        // Only a base type inlines. A nested `Cell` or `Wrapping` stays visible.
        if rule.inline_leaf
            && matches!(inner.type_name, VariableType::Base(_))
            && !cache.has_children(inner)
            && inner.value.is_valid()
            && !inner.value.is_empty()
        {
            variable.set_value(match inner.value.clone() {
                VariableValue::Valid(value) => {
                    VariableValue::Valid(format!("{} = {value}", variable.compact_type_name()))
                }
                VariableValue::Error(value) => VariableValue::Error(value),
                VariableValue::Empty => VariableValue::Empty,
            });
            cache.remove_cache_entry(inner.variable_key)?;
            variable.variable_node_type = VariableNodeType::DoNotRecurse;
            cache.update_variable(variable)?;
        }

        Ok(())
    }

    /// Shows the initialized prefix of a `heapless::Vec` buffer as a slice.
    #[expect(clippy::too_many_arguments)]
    fn expand_heapless_vec(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(path) = RustPath::from_die(unit_info, debug_info, node) else {
            return Ok(());
        };
        if !path.is_heapless_vec() {
            return Ok(());
        }

        let children: Vec<_> = cache.get_children(variable.variable_key).cloned().collect();
        let Some(len) = children
            .iter()
            .find(|c| is_named(c, "len"))
            .and_then(|field| match &field.value {
                VariableValue::Valid(len) => len.parse::<u64>().ok(),
                _ => None,
            })
        else {
            return Ok(());
        };
        let Some(mut buffer) = children.iter().find(|c| is_named(c, "buffer")).cloned() else {
            return Ok(());
        };

        debug_info.cache_deferred_variables(cache, memory, &mut buffer, frame_info)?;
        let Some(buffer_field) = cache.get_variable_by_key(buffer.variable_key) else {
            return Ok(());
        };
        let Some(array) =
            Self::storage_array(debug_info, memory, cache, frame_info, buffer_field.clone())?
        else {
            return Ok(());
        };

        let elements: Vec<_> = cache
            .get_children(array.variable_key)
            .filter(|c| matches!(c.name, VariableName::Indexed(_)))
            .cloned()
            .collect();

        for element in &elements {
            let VariableName::Indexed(index) = element.name else {
                continue;
            };
            if index >= len {
                cache.remove_cache_entry(element.variable_key)?;
            }
        }

        let live = cache
            .get_children(array.variable_key)
            .filter(|c| matches!(c.name, VariableName::Indexed(index) if index < len))
            .cloned()
            .collect::<Vec<_>>();

        for mut element in live {
            self.unwrap_storage_slot(debug_info, memory, cache, frame_info, &mut element)?;
        }

        let buffer_key = if array.variable_key != buffer_field.variable_key {
            cache.adopt_grand_children(&buffer_field, &array)?;
            buffer_field.variable_key
        } else {
            array.variable_key
        };
        let Some(mut buffer) = cache.get_variable_by_key(buffer_key) else {
            return Ok(());
        };

        if let Some(first) = cache.get_children(buffer.variable_key).next().cloned() {
            buffer.type_name = VariableType::Array {
                item_type_name: Box::new(first.type_name.clone()),
                count: len as usize,
            };
            if let Some(item_size) = first.byte_size {
                buffer.byte_size = Some(item_size * len);
            }
        } else if let VariableType::Array { count, .. } = &mut buffer.type_name {
            *count = 0;
            buffer.byte_size = Some(0);
        }
        buffer.value = VariableValue::Empty;
        cache.update_variable(&buffer)?;

        Ok(())
    }

    /// Moves captured locals out of a compiler-generated async environment into the frame scope.
    fn promote_async_env_locals(
        &self,
        debug_info: &DebugInfo,
        scope: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let params: Vec<_> = cache.get_children(scope.variable_key).cloned().collect();
        let Some(env_param) = params
            .into_iter()
            .find(|child| looks_like_async_env_param(debug_info, child))
        else {
            return Ok(());
        };

        let Some(payload) =
            self.async_env_payload(debug_info, memory, cache, frame_info, env_param.clone())?
        else {
            return Ok(());
        };

        let existing: Vec<_> = cache
            .get_children(scope.variable_key)
            .filter_map(|child| match &child.name {
                VariableName::Named(name) => Some(name.clone()),
                _ => None,
            })
            .collect();

        let locals: Vec<_> = cache.get_children(payload.variable_key).cloned().collect();
        let mut awaitee = None;
        let mut resume_ty = None;
        let mut context_refs = Vec::new();
        for mut local in locals {
            if is_resume_ty(debug_info, &local) {
                resume_ty = Some(local);
                continue;
            }
            if is_poll_context_ref(debug_info, &local) {
                context_refs.push(local);
                continue;
            }
            let VariableName::Named(name) = &local.name else {
                continue;
            };
            if name == "__awaitee" {
                awaitee = Some(local);
                continue;
            }
            if name == "_task_context" {
                if is_resume_ty(debug_info, &local) {
                    resume_ty = Some(local);
                } else {
                    context_refs.push(local);
                }
                continue;
            }
            if name.starts_with("__") || existing.iter().any(|existing| existing == name) {
                continue;
            }
            local.parent_key = scope.variable_key;
            cache.update_variable(&local)?;
        }

        // rust-lang/rust#157166: `poll` keeps `&mut Context`, and the env keeps `ResumeTy`
        // as `ResumeTy(transmute(context))`. Older rustc rewrote the env slot to `&mut Context`.
        let frame_children: Vec<_> = cache.get_children(scope.variable_key).cloned().collect();
        for child in &frame_children {
            if is_resume_ty(debug_info, child)
                && resume_ty
                    .as_ref()
                    .is_none_or(|existing| existing.variable_key != child.variable_key)
            {
                resume_ty = Some(child.clone());
            }
        }

        let task_contexts = if let Some(resume_ty) = resume_ty {
            for child in &frame_children {
                if is_poll_context_ref(debug_info, child) {
                    cache.remove_cache_entry(child.variable_key)?;
                }
            }
            vec![resume_ty]
        } else {
            let mut contexts = context_refs;
            for child in frame_children {
                if !is_poll_context_ref(debug_info, &child) {
                    continue;
                }
                if contexts
                    .iter()
                    .any(|context| context.variable_key == child.variable_key)
                {
                    continue;
                }
                contexts.push(child);
            }
            contexts
        };

        const FUTURE_STATE: &str = "Future state";

        if (is_generator_variant(&payload) || awaitee.is_some() || !task_contexts.is_empty())
            && !existing.iter().any(|name| name == FUTURE_STATE)
        {
            let mut future_state = cache.create_variable(scope.variable_key, None)?;
            future_state.name = VariableName::Named(FUTURE_STATE.to_string());
            future_state.type_name = payload.type_name.clone();
            future_state.source_location = payload.source_location.clone();
            future_state.memory_location = payload.memory_location.clone();
            future_state.byte_size = payload.byte_size;
            future_state.set_value(VariableValue::Valid(payload.compact_type_name()));

            let mut has_child = false;
            if let Some(mut awaitee) = awaitee {
                awaitee.parent_key = future_state.variable_key;
                cache.update_variable(&awaitee)?;
                has_child = true;
            }
            for mut task_context in task_contexts {
                task_context.parent_key = future_state.variable_key;
                cache.update_variable(&task_context)?;
                has_child = true;
            }
            if !has_child {
                future_state.variable_node_type = VariableNodeType::DoNotRecurse;
            }
            cache.update_variable(&future_state)?;
        }

        cache.remove_cache_entry(env_param.variable_key)?;
        Ok(())
    }

    fn async_env_payload(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
        mut current: Variable,
    ) -> Result<Option<Variable>, DebugError> {
        for _ in 0..8 {
            debug_info.cache_deferred_variables(cache, memory, &mut current, frame_info)?;
            let Some(current_now) = cache.get_variable_by_key(current.variable_key) else {
                return Ok(None);
            };
            current = current_now;

            if matches!(current.type_name, VariableType::Pointer(_)) {
                if cache
                    .get_children(current.variable_key)
                    .any(is_generator_variant)
                {
                    return self
                        .async_suspend_payload(debug_info, memory, cache, frame_info, current);
                }
                let Some(pointee) = cache.get_children(current.variable_key).next().cloned() else {
                    return Ok(None);
                };
                current = pointee;
                continue;
            }

            if is_pin_type(debug_info, &current) {
                let Some(pointer) = cache
                    .get_children(current.variable_key)
                    .find(|child| matches!(child.type_name, VariableType::Pointer(_)))
                    .cloned()
                else {
                    return Ok(None);
                };
                current = pointer;
                continue;
            }

            if is_async_env_type(debug_info, &current) {
                return self.async_suspend_payload(debug_info, memory, cache, frame_info, current);
            }

            return Ok(None);
        }

        Ok(None)
    }

    fn async_suspend_payload(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
        mut env: Variable,
    ) -> Result<Option<Variable>, DebugError> {
        debug_info.cache_deferred_variables(cache, memory, &mut env, frame_info)?;
        let Some(env) = cache.get_variable_by_key(env.variable_key) else {
            return Ok(None);
        };

        let children: Vec<_> = cache.get_children(env.variable_key).cloned().collect();
        for mut child in children {
            if !is_generator_variant(&child) {
                continue;
            }
            debug_info.cache_deferred_variables(cache, memory, &mut child, frame_info)?;
            let Some(child) = cache.get_variable_by_key(child.variable_key) else {
                continue;
            };
            if cache.has_children(&child) {
                return Ok(Some(child));
            }
        }

        Ok(Some(env))
    }

    fn storage_array(
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
        mut buffer: Variable,
    ) -> Result<Option<Variable>, DebugError> {
        // Walk down a few levels to cover both old and new heapless::Vec layouts
        for _ in 0..4 {
            if matches!(buffer.type_name, VariableType::Array { .. }) {
                return Ok(Some(buffer));
            }

            debug_info.cache_deferred_variables(cache, memory, &mut buffer, frame_info)?;
            let Some(current) = cache.get_variable_by_key(buffer.variable_key) else {
                return Ok(None);
            };
            let inner: Vec<_> = cache.get_children(current.variable_key).cloned().collect();
            let [inner] = inner.as_slice() else {
                return Ok(None);
            };
            let mut inner = inner.clone();
            debug_info.cache_deferred_variables(cache, memory, &mut inner, frame_info)?;
            let Some(inner) = cache.get_variable_by_key(inner.variable_key) else {
                return Ok(None);
            };
            buffer = inner;
        }

        Ok(None)
    }

    fn unwrap_storage_slot(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
        element: &mut Variable,
    ) -> Result<(), DebugError> {
        for _ in 0..8 {
            if !self.unwrap_storage_wrapper(debug_info, memory, cache, frame_info, element)? {
                break;
            }
            let Some(updated) = cache.get_variable_by_key(element.variable_key) else {
                return Ok(());
            };
            *element = updated;
        }

        let Some(mut element) = cache.get_variable_by_key(element.variable_key) else {
            return Ok(());
        };
        let Some(offset) = element.type_node_offset else {
            return Ok(());
        };
        let Ok((unit_info, entry)) = debug_info.entry_at_debug_info_offset(offset) else {
            return Ok(());
        };
        self.flatten_known_wrapper(
            unit_info,
            debug_info,
            &entry,
            &mut element,
            memory,
            cache,
            frame_info,
        )?;
        let Some(mut element) = cache.get_variable_by_key(element.variable_key) else {
            return Ok(());
        };
        if element.value.is_valid() && !element.value.is_empty() {
            cache.remove_cache_entry_children(element.variable_key)?;
            element.variable_node_type = VariableNodeType::DoNotRecurse;
            cache.update_variable(&element)?;
        }
        Ok(())
    }

    fn unwrap_storage_wrapper(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
        element: &mut Variable,
    ) -> Result<bool, DebugError> {
        debug_info.cache_deferred_variables(cache, memory, element, frame_info)?;
        let Some(element_now) = cache.get_variable_by_key(element.variable_key) else {
            return Ok(false);
        };
        let Some(path) = RustPath::from_variable(debug_info, &element_now) else {
            return Ok(false);
        };
        if !path.is_storage_wrapper() {
            return Ok(false);
        }

        let children: Vec<_> = cache
            .get_children(element_now.variable_key)
            .cloned()
            .collect();
        let Some(mut value) = storage_payload(&children).cloned() else {
            return Ok(false);
        };

        debug_info.cache_deferred_variables(cache, memory, &mut value, frame_info)?;
        let Some(value) = cache.get_variable_by_key(value.variable_key) else {
            return Ok(false);
        };

        let mut element = element_now;
        element.type_name = value.type_name.clone();
        element.type_node_offset = value.type_node_offset;
        element.byte_size = value.byte_size;

        if cache.has_children(&value) {
            for child in &children {
                if child.variable_key != value.variable_key {
                    cache.remove_cache_entry(child.variable_key)?;
                }
            }
            cache.adopt_grand_children(&element, &value)?;
            element.value = VariableValue::Empty;
        } else {
            element.set_value(value.value.clone());
            cache.remove_cache_entry_children(element.variable_key)?;
            element.variable_node_type = VariableNodeType::DoNotRecurse;
        }

        cache.update_variable(&element)?;
        Ok(true)
    }

    /// The value of a `&str` or a slice is read through its data pointer, so the pointer must be
    /// resolved before the value of the enclosing variable is read.
    fn resolve_data_pointer(
        &self,
        debug_info: &DebugInfo,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(mut data_ptr) = cache
            .get_children(variable.variable_key)
            .find(|child| is_named(child, "data_ptr"))
            .cloned()
        else {
            return Ok(());
        };

        debug_info.cache_deferred_variables(cache, memory, &mut data_ptr, frame_info)
    }

    /// Replaces *const data pointer with *const [data; len] in slices.
    ///
    /// This function may return `Ok(())` even if it does not modify the variable.
    fn expand_slice(
        &self,
        debug_info: &DebugInfo,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(slice) = Self::try_get_slice(variable, cache) else {
            return Ok(());
        };

        // Turn the data pointer into an array.
        let pointer_key = slice.data_ptr.variable_key;
        let length = slice.length;

        let Some(mut pointee) = cache.get_children(pointer_key).next().cloned() else {
            return Ok(());
        };

        // Do we know the type of the data?
        let Some(type_node_offset) = pointee.type_node_offset else {
            return Ok(());
        };

        // Let's just remove the pointer. While it may be interesting where the data is, the
        // address can be read using the debugger, and is otherwise just noise on the UI.
        cache.remove_cache_entry(pointer_key)?;

        // Replace the pointee type with an array of known length. We don't have to modify the
        // memory location, as the pointer is already pointing to the first element of the array.
        pointee.parent_key = ObjectRef::Invalid;
        pointee.variable_key = ObjectRef::Invalid;
        pointee.value = VariableValue::Empty;
        pointee.type_name = VariableType::Array {
            item_type_name: { Box::new(pointee.type_name) },
            count: length as usize,
        };
        pointee.variable_node_type = VariableNodeType::RecurseToBaseType;

        cache.add_variable(variable.variable_key, &mut pointee)?;

        let (member_unit, array_member_type_node) =
            debug_info.entry_at_debug_info_offset(type_node_offset)?;

        let member_range = 0..length;
        member_unit.expand_array_members(
            debug_info,
            &array_member_type_node,
            cache,
            &mut pointee,
            memory,
            &[member_range],
            frame_info,
        )?;

        Ok(())
    }
}

fn storage_payload(children: &[Variable]) -> Option<&Variable> {
    if let Some(value) = children.iter().find(|child| is_named(child, "value")) {
        return Some(value);
    }

    let rest: Vec<_> = children
        .iter()
        .filter(|child| !is_named(child, "uninit"))
        .collect();
    match rest.as_slice() {
        [payload] => Some(*payload),
        _ => None,
    }
}

fn looks_like_async_env_param(debug_info: &DebugInfo, variable: &Variable) -> bool {
    // `poll` names its `Pin<&mut {async_fn_env}>` argument `future`. The compiler
    // argument on the async fn itself has no name.
    if matches!(variable.name, VariableName::Named(_)) {
        return false;
    }
    is_async_env_type(debug_info, variable) || type_contains_async_env(&variable.type_name())
}

fn type_contains_async_env(type_name: &str) -> bool {
    let mut rest = type_name;
    for _ in 0..8 {
        if is_async_env_ident(rest) {
            return true;
        }
        if let Some(inner) = rest.strip_prefix("&mut ") {
            rest = inner;
            continue;
        }
        if let Some(inner) = rest.strip_prefix('&') {
            rest = inner;
            continue;
        }
        if let Some(inner) = rest.strip_prefix("*mut ") {
            rest = inner;
            continue;
        }
        if let Some(inner) = rest.strip_prefix("*const ") {
            rest = inner;
            continue;
        }
        let Some((_, inner)) = rest.split_once('<') else {
            return false;
        };
        rest = inner.trim_end_matches('>').trim();
    }
    false
}

fn is_async_env_type(debug_info: &DebugInfo, variable: &Variable) -> bool {
    if let Some(path) = RustPath::from_variable(debug_info, variable) {
        return is_async_env_ident(&path.ident);
    }
    is_async_env_ident(&variable.type_name())
}

fn is_pin_type(debug_info: &DebugInfo, variable: &Variable) -> bool {
    if let Some(path) = RustPath::from_variable(debug_info, variable) {
        return path.ident == "Pin" && path.is_rustc_lib();
    }
    type_ident(&variable.type_name()) == "Pin"
}

fn is_async_env_ident(type_name: &str) -> bool {
    let ident = type_ident(type_name);
    ident.starts_with("{async_fn_env")
        || ident.starts_with("{async_block_env")
        || ident.starts_with("{async_closure_env")
}

fn is_generator_variant(variable: &Variable) -> bool {
    is_generator_variant_ident(&variable.type_name())
}

fn is_generator_variant_ident(type_name: &str) -> bool {
    let ident = type_ident(type_name);
    ident.starts_with("Suspend") || matches!(ident, "Unresumed" | "Returned" | "Panicked")
}

fn is_resume_ty(debug_info: &DebugInfo, variable: &Variable) -> bool {
    if let Some(path) = RustPath::from_variable(debug_info, variable)
        && path.ident == "ResumeTy"
    {
        return true;
    }
    type_ident(&variable.type_name()) == "ResumeTy"
}

fn is_poll_context_ref(debug_info: &DebugInfo, variable: &Variable) -> bool {
    if is_resume_ty(debug_info, variable) {
        return false;
    }
    if let Some(path) = RustPath::from_variable(debug_info, variable)
        && path.ident == "Context"
        && path.crate_name == "core"
        && path.modules_eq(&["task", "wake"])
    {
        return true;
    }
    variable.type_name().contains("task::wake::Context")
}

fn collect_single_child_chain(
    debug_info: &DebugInfo,
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
    cache: &mut VariableCache,
    frame_info: &StackFrameInfo<'_>,
) -> Result<Vec<Variable>, DebugError> {
    let mut inners = Vec::new();
    let mut current = variable.clone();
    for _ in 0..8 {
        let children: Vec<_> = cache.get_children(current.variable_key).cloned().collect();
        let [inner] = children.as_slice() else {
            break;
        };
        let mut inner = inner.clone();
        debug_info.cache_deferred_variables(cache, memory, &mut inner, frame_info)?;
        let Some(inner) = cache.get_variable_by_key(inner.variable_key) else {
            break;
        };
        inners.push(inner.clone());
        current = inner;
    }
    Ok(inners)
}

pub(super) const STDLIB: &str = "core|alloc|std";
const CORE_CELL: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "cell"]),
    ident: Ident::Exact("Cell"),
};
const CORE_SYNC_UNSAFE_CELL: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "cell"]),
    ident: Ident::Exact("SyncUnsafeCell"),
};
const CORE_UNSAFE_CELL: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "cell"]),
    ident: Ident::Exact("UnsafeCell"),
};
const CORE_MANUALLY_DROP: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "mem"]),
    ident: Ident::Exact("ManuallyDrop"),
};
const CORE_MAYBE_DANGLING: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "mem"]),
    ident: Ident::Exact("MaybeDangling"),
};
const CORE_WRAPPING: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "num"]),
    ident: Ident::Exact("Wrapping"),
};
const CORE_NON_ZERO: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "num"]),
    ident: Ident::Exact("NonZero"),
};
const CORE_ATOMIC: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "sync", "atomic"]),
    ident: Ident::Prefix("Atomic"),
};
const CORE_ATOMIC_ALIGN: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&[STDLIB, "sync", "atomic"]),
    ident: Ident::Prefix("Align"),
};
const EMBASSY_SYNC_UNSAFE_CELL: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&["embassy_executor", "raw", "util"]),
    ident: Ident::Exact("SyncUnsafeCell"),
};
const PORTABLE_ATOMIC: TypeMatch = TypeMatch {
    modules: ModulePath::Prefix(&["portable_atomic"]),
    ident: Ident::Prefix("Atomic"),
};

/// `Cell - UnsafeCell - T` becomes `Cell - T`. `Atomic* - Align* - UnsafeCell - T` becomes
/// `Atomic* - T`. The outer type stays when it is useful.
const WRAPPER_CHAINS: &[WrapperChain] = &[
    WrapperChain {
        outer: CORE_CELL,
        inners: &[InnerLayer::Skip(CORE_UNSAFE_CELL), InnerLayer::Payload],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: CORE_SYNC_UNSAFE_CELL,
        inners: &[InnerLayer::Skip(CORE_UNSAFE_CELL), InnerLayer::Payload],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: CORE_MANUALLY_DROP,
        inners: &[
            InnerLayer::SkipOpt(CORE_MAYBE_DANGLING),
            InnerLayer::SkipOpt(CORE_UNSAFE_CELL),
            InnerLayer::Payload,
        ],
        inline_leaf: true,
        hide_outer: true,
    },
    WrapperChain {
        outer: CORE_MAYBE_DANGLING,
        inners: &[
            InnerLayer::SkipOpt(CORE_MANUALLY_DROP),
            InnerLayer::SkipOpt(CORE_UNSAFE_CELL),
            InnerLayer::Payload,
        ],
        inline_leaf: true,
        hide_outer: true,
    },
    WrapperChain {
        outer: CORE_WRAPPING,
        inners: &[InnerLayer::Payload],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: CORE_NON_ZERO,
        inners: &[InnerLayer::Payload],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: CORE_ATOMIC,
        inners: &[
            InnerLayer::SkipOpt(CORE_ATOMIC_ALIGN),
            InnerLayer::SkipOpt(CORE_UNSAFE_CELL),
            InnerLayer::SkipOpt(CORE_ATOMIC_ALIGN),
            InnerLayer::Payload,
        ],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: EMBASSY_SYNC_UNSAFE_CELL,
        inners: &[InnerLayer::Skip(CORE_UNSAFE_CELL), InnerLayer::Payload],
        inline_leaf: true,
        hide_outer: false,
    },
    WrapperChain {
        outer: TypeMatch {
            modules: ModulePath::Exact(&["vcell"]),
            ident: Ident::Exact("VolatileCell"),
        },
        inners: &[InnerLayer::Skip(CORE_UNSAFE_CELL), InnerLayer::Payload],
        inline_leaf: false,
        hide_outer: false,
    },
    WrapperChain {
        outer: PORTABLE_ATOMIC,
        inners: &[
            InnerLayer::SkipOpt(PORTABLE_ATOMIC),
            InnerLayer::SkipOpt(CORE_ATOMIC),
            InnerLayer::SkipOpt(CORE_ATOMIC_ALIGN),
            InnerLayer::SkipOpt(CORE_UNSAFE_CELL),
            InnerLayer::SkipOpt(CORE_ATOMIC_ALIGN),
            InnerLayer::Payload,
        ],
        inline_leaf: true,
        hide_outer: false,
    },
];

pub(crate) fn process_struct(
    unit_info: &UnitInfo,
    debug_info: &DebugInfo,
    node: &gimli::DebuggingInformationEntry<GimliReader, usize>,
    variable: &mut Variable,
    memory: &mut dyn MemoryInterface,
    cache: &mut VariableCache,
    frame_info: &StackFrameInfo<'_>,
) -> Result<(), DebugError> {
    Rust.resolve_data_pointer(debug_info, variable, memory, cache, frame_info)?;

    if variable
        .type_name
        .ident()
        .is_some_and(|ident| ident.starts_with("&["))
    {
        Rust.expand_slice(debug_info, variable, memory, cache, frame_info)?;
    }

    Rust.flatten_known_wrapper(
        unit_info, debug_info, node, variable, memory, cache, frame_info,
    )?;
    Rust.expand_heapless_vec(
        unit_info, debug_info, node, variable, memory, cache, frame_info,
    )?;
    if matches!(variable.name, VariableName::LocalScopeRoot) {
        Rust.promote_async_env_locals(debug_info, variable, memory, cache, frame_info)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::type_ident, CORE_ATOMIC_ALIGN, CORE_UNSAFE_CELL, RustPath, WRAPPER_CHAINS,
        is_async_env_ident, is_generator_variant_ident, type_contains_async_env,
    };
    use crate::DebugInfo;

    /// `type_name`, `prefix::type_name`, or those names with a generic argument list.
    fn is_rust_type(name: &str, type_name: &str) -> bool {
        type_ident(name) == type_name
    }

    #[test]
    fn rust_type_name_matches() {
        assert!(is_rust_type("Option", "Option"));
        assert!(is_rust_type(
            "Option<&mut probe_rs_debugger_test::RecursiveStruct>",
            "Option"
        ));
        assert!(is_rust_type("core::option::Option<u32>", "Option"));
        assert!(!is_rust_type("Error", "Err"));
        assert!(!is_rust_type("Optional<u32>", "Option"));
        assert!(is_rust_type("Result<T, E>", "Result"));
        assert!(is_rust_type("UnsafeCell<u32>", "UnsafeCell"));
    }

    #[test]
    fn async_env_type_names_are_compiler_generated() {
        assert!(is_async_env_ident("{async_fn_env#0}"));
        assert!(is_async_env_ident("s3_debug::__main::{async_fn_env#0}"));
        assert!(!is_async_env_ident(
            "Pin<&mut s3_debug::__main::{async_fn_env#0}>"
        ));
        assert!(type_contains_async_env(
            "Pin<&mut s3_debug::__main::{async_fn_env#0}>"
        ));
        assert!(is_generator_variant_ident("Suspend0"));
        assert!(is_generator_variant_ident("Unresumed"));
        assert!(!is_generator_variant_ident("Spawner"));
    }

    #[test]
    fn rustc_and_embassy_wrappers_match_their_namespaces() {
        assert!(starts_wrapper_chain(&path("core", &["cell"], "Cell")));
        assert!(!starts_wrapper_chain(&path(
            "core",
            &["cell"],
            "UnsafeCell"
        )));
        assert!(CORE_UNSAFE_CELL.matches(&path("core", &["cell"], "UnsafeCell")));
        assert!(starts_wrapper_chain(&path(
            "core",
            &["sync", "atomic"],
            "AtomicU32"
        )));
        assert!(starts_wrapper_chain(&path(
            "core",
            &["mem", "manually_drop"],
            "ManuallyDrop"
        )));
        assert!(starts_wrapper_chain(&path(
            "core",
            &["mem", "maybe_dangling"],
            "MaybeDangling"
        )));
        assert!(starts_wrapper_chain(&path(
            "core",
            &["num", "wrapping"],
            "Wrapping"
        )));
        assert!(starts_wrapper_chain(&path(
            "core",
            &["num", "nonzero"],
            "NonZero"
        )));
        assert!(starts_wrapper_chain(&path(
            "embassy_executor",
            &["raw", "util"],
            "SyncUnsafeCell"
        )));

        assert!(starts_wrapper_chain(&path(
            "portable_atomic",
            &[],
            "AtomicU32"
        )));
        assert!(starts_wrapper_chain(&path(
            "portable_atomic",
            &["imp", "core_atomic"],
            "AtomicU32"
        )));
        assert!(starts_wrapper_chain(&path(
            "portable_atomic",
            &[],
            "Atomic"
        )));
        assert!(!starts_wrapper_chain(&path(
            "core",
            &["sync", "atomic", "private"],
            "Align4"
        )));
        assert!(CORE_ATOMIC_ALIGN.matches(&path("core", &["sync", "atomic", "private"], "Align4")));
        assert!(!starts_wrapper_chain(&path(
            "embassy_sync",
            &["waitqueue", "atomic_waker"],
            "AtomicWaker"
        )));
        assert!(!starts_wrapper_chain(&path(
            "esp_hal",
            &["sync", "multicore"],
            "AtomicLock"
        )));
        assert!(!starts_wrapper_chain(&path("core", &["cell"], "RefCell")));
        assert!(!starts_wrapper_chain(&path("core", &["pin"], "Pin")));
        assert!(!starts_wrapper_chain(&path(
            "core",
            &["ptr", "non_null"],
            "NonNull"
        )));
        assert!(path("heapless", &["vec"], "Vec").is_heapless_vec());
        assert!(path("heapless", &["vec"], "VecInner").is_heapless_vec());
        assert!(!path("heapless", &["vec", "storage"], "VecStorageInner").is_heapless_vec());
        assert!(!path("alloc", &["vec"], "Vec").is_heapless_vec());
        assert!(path("core", &["mem", "maybe_uninit"], "MaybeUninit").is_storage_wrapper());
        assert!(path("core", &["mem", "manually_drop"], "ManuallyDrop").is_storage_wrapper());
        assert!(path("core", &["mem", "maybe_dangling"], "MaybeDangling").is_storage_wrapper());
        assert!(!path("core", &["cell"], "Cell").is_storage_wrapper());
        assert!(starts_wrapper_chain(&path("vcell", &[], "VolatileCell")));
        assert!(path("vcell", &[], "VolatileCell").is_volatile_cell());
        assert!(!path("core", &["cell"], "UnsafeCell").is_volatile_cell());
    }

    #[test]
    fn wrapper_chains_skip_implementation_layers() {
        assert_eq!(
            chain_skips(&[
                path("core", &["cell"], "Cell"),
                path("core", &["cell"], "UnsafeCell"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0])
        );
        assert_eq!(
            chain_skips(&[
                path("core", &["cell"], "Cell"),
                path("core", &["num", "wrapping"], "Wrapping"),
            ]),
            None
        );
        assert_eq!(
            chain_skips(&[
                path("core", &["sync", "atomic"], "AtomicU32"),
                path("core", &["sync", "atomic", "private"], "Align4"),
                path("core", &["cell"], "UnsafeCell"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0, 1])
        );
        assert_eq!(
            chain_skips(&[
                path("core", &["sync", "atomic"], "AtomicU32"),
                path("core", &["cell"], "UnsafeCell"),
                path("core", &["sync", "atomic", "private"], "Align4"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0, 1])
        );
        assert_eq!(
            chain_skips(&[
                path("core", &["sync", "atomic"], "AtomicU32"),
                path("core", &["cell"], "UnsafeCell"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0])
        );
        assert_eq!(
            chain_skips(&[
                path("portable_atomic", &[], "AtomicU32"),
                path("core", &["cell"], "UnsafeCell"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0])
        );
        assert_eq!(
            chain_skips(&[
                path("portable_atomic", &[], "AtomicU32"),
                path("portable_atomic", &["imp", "core_atomic"], "AtomicU32"),
                path("core", &["sync", "atomic"], "Atomic"),
                path("core", &["option"], "Option"),
            ]),
            Some(vec![0, 1])
        );
        assert_eq!(
            chain_skips(&[path("core", &["sync", "atomic", "private"], "Align4")]),
            None
        );
    }

    #[test]
    fn dwarf_namespaces_identify_the_crate() {
        let elf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/debug-unwind-tests/esp32s3_esp_hal_panic.elf");
        let debug_info = DebugInfo::from_file(&elf).unwrap();

        let mut saw_core_cell = false;
        let mut saw_core_atomic = false;
        let mut saw_portable_atomic = false;
        for unit in &debug_info.unit_infos {
            let mut cursor = unit.unit.entries();
            while let Ok(Some(entry)) = cursor.next_dfs() {
                if entry.tag() != gimli::DW_TAG_structure_type {
                    continue;
                }
                let Some(path) = RustPath::from_die(unit, &debug_info, entry) else {
                    continue;
                };
                if path.ident == "Cell" && path.crate_name == "core" {
                    saw_core_cell = true;
                    assert_eq!(path.modules, ["cell"]);
                    assert!(starts_wrapper_chain(&path));
                }
                if path.ident == "AtomicU32" && path.crate_name == "core" {
                    saw_core_atomic = true;
                    assert_eq!(path.modules, ["sync", "atomic"]);
                    assert!(starts_wrapper_chain(&path));
                }
                if path.ident == "AtomicU32" && path.crate_name == "portable_atomic" {
                    saw_portable_atomic = true;
                    assert!(starts_wrapper_chain(&path));
                }
            }
        }
        assert!(saw_core_cell);
        assert!(saw_core_atomic);
        assert!(saw_portable_atomic);
    }

    fn starts_wrapper_chain(path: &RustPath) -> bool {
        WRAPPER_CHAINS.iter().any(|chain| chain.outer.matches(path))
    }

    fn chain_skips(chain: &[RustPath]) -> Option<Vec<usize>> {
        let (outer, inners) = chain.split_first()?;
        let inners: Vec<_> = inners.iter().cloned().map(Some).collect();
        WRAPPER_CHAINS.iter().find_map(|rule| {
            if !rule.outer.matches(outer) {
                return None;
            }
            rule.skip_inner_indices(&inners)
        })
    }

    fn path(crate_name: &str, modules: &[&str], ident: &str) -> RustPath {
        RustPath {
            crate_name: crate_name.to_string(),
            modules: modules.iter().map(|s| (*s).to_string()).collect(),
            ident: ident.to_string(),
        }
    }
}
