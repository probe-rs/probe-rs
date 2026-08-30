use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use gimli::{AttributeValue, UnitOffset};
use probe_rs::MemoryInterface;

use super::{
    AttributesEntry, DebugError, DebugInfo, GimliReader, StackFrameInfo, UnitInfo, VariableCache,
    die_contains_pc, extract_name,
};
use crate::{
    ObjectRef,
    variable::{
        Modifier, Variable, VariableName, VariableNodeType, VariableType, VariableValue,
        VariantRole,
    },
};

/// A type with more members than this does not consume the expansion budget. A chain of
/// transparent wrappers must expand in full, so that language post-processing can remove it.
const NARROW_MEMBERS: usize = 2;

/// The number of wide types that a single walk expands.
const WIDE_BUDGET: usize = 32;

/// The number of types that a single walk expands, whatever their width. A type that holds two
/// members of a type that holds two members grows without a limit of this kind.
const TOTAL_BUDGET: usize = 4096;

/// Limits how many types a single walk expands without a request from the debugger client.
struct Budget {
    wide: usize,
    total: usize,
}

impl Budget {
    /// Report whether a type of `width` members may expand, and charge it if it may. A narrow type
    /// does not draw on the `wide` budget, so that a chain of transparent wrappers reaches the
    /// type that it wraps however deep the chain is.
    fn affords(&mut self, width: usize) -> bool {
        if self.total == 0 {
            return false;
        }
        if width > NARROW_MEMBERS {
            if self.wide == 0 {
                return false;
            }
            self.wide -= 1;
        }
        self.total -= 1;
        true
    }
}

/// Count the members that an expansion of `node` adds. A `DW_TAG_variant_part` adds the members of
/// whichever variant is active, so it never counts as narrow.
fn expansion_width(
    unit: &UnitInfo,
    node: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Result<usize, DebugError> {
    let mut tree = unit.unit.entries_tree(Some(node.offset()))?;
    let mut children = tree.root()?.children();
    let mut width = 0;
    while let Some(child) = children.next()? {
        width += match child.entry().tag() {
            gimli::DW_TAG_member => 1,
            gimli::DW_TAG_variant_part => NARROW_MEMBERS + 1,
            _ => 0,
        };
    }
    Ok(width)
}

// TODO: This is language specific, and should be moved to the language implementations.
fn target_name(pointer_name: &VariableName) -> VariableName {
    match pointer_name {
        VariableName::Named(name) if name.starts_with("Some ") => {
            VariableName::Named(name.replacen('&', "*", 1))
        }
        VariableName::Named(name) => VariableName::Named(format!("*{name}")),
        other => VariableName::Named(format!(
            "Error: Unable to generate name, parent variable does not have a name but is special variable {other:?}"
        )),
    }
}

struct AttrResume<'a> {
    unit: &'a UnitInfo,
    die_offset: UnitOffset,
    attributes_offset: Option<UnitOffset>,
    parent_key: ObjectRef,
    child_key: ObjectRef,
    skip: usize,
}

struct ArrayExpansion<'a> {
    member_unit: &'a UnitInfo,
    type_offset: UnitOffset,
    array_key: ObjectRef,
    member_index: u64,
    explode_start: u64,
    explode_end: u64,
    current_range_empty: bool,
    remaining_ranges: Vec<Range<u64>>,
}

enum Job<'a> {
    ProcessTree {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    FinishTree {
        unit: &'a UnitInfo,
        parent_key: ObjectRef,
    },
    ProcessVariable {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    ProcessVariableFinish {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        child_key: ObjectRef,
    },
    ProcessNamespace {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    DropEmptyNamespace(ObjectRef),
    ProcessLexicalBlock {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    ProcessVariantPart {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    ProcessVariantPartFinish {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        dummy_key: ObjectRef,
    },
    ProcessVariantNode {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    ProcessVariant {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    },
    ProcessVariantFinish {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    },
    AdoptGrandChildren {
        parent_key: ObjectRef,
        child_key: ObjectRef,
    },
    ResumeNodeAttributes(AttrResume<'a>),
    ApplyDiscriminant {
        parent_key: ObjectRef,
        discriminant_key: ObjectRef,
    },
    ExtractType {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    },
    ExtractValue {
        key: ObjectRef,
    },
    ApplyModifier {
        key: ObjectRef,
        modifier: Modifier,
    },
    RestoreNodeType {
        key: ObjectRef,
        node_type: VariableNodeType,
    },
    ProcessStruct {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        key: ObjectRef,
    },
    RemoveIfUnit {
        key: ObjectRef,
    },
    ExpandArray {
        member_unit: &'a UnitInfo,
        type_offset: UnitOffset,
        array_key: ObjectRef,
        subranges: Vec<Range<u64>>,
    },
    ExpandArrayElement(ArrayExpansion<'a>),
    ExpandArrayAfterElement {
        expansion: ArrayExpansion<'a>,
        member_key: ObjectRef,
    },
    VisitStatics {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
    VisitNamespace {
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    },
}

pub(super) struct Walker<'a> {
    debug_info: &'a DebugInfo,
    memory: &'a mut dyn MemoryInterface,
    cache: &'a mut VariableCache,
    frame_info: &'a StackFrameInfo<'a>,
    jobs: Vec<Job<'a>>,
    default_variants: HashMap<ObjectRef, UnitOffset>,
    budget: Budget,
}

impl<'a> Walker<'a> {
    pub(super) fn new(
        debug_info: &'a DebugInfo,
        memory: &'a mut dyn MemoryInterface,
        cache: &'a mut VariableCache,
        frame_info: &'a StackFrameInfo<'a>,
    ) -> Self {
        Self {
            debug_info,
            memory,
            cache,
            frame_info,
            jobs: Vec::new(),
            default_variants: HashMap::new(),
            budget: Budget {
                wide: WIDE_BUDGET,
                total: TOTAL_BUDGET,
            },
        }
    }

    /// Walk the DIE tree below `offset` and add every descendant variable to the cache.
    pub(super) fn tree(
        mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        self.jobs.push(Job::ProcessTree {
            unit,
            offset,
            parent_key,
        });
        self.run()
    }

    /// Walk the compilation unit below `offset` and add the static variables it declares.
    pub(super) fn statics(
        mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        self.jobs.push(Job::VisitStatics {
            unit,
            offset,
            parent_key,
        });
        self.run()
    }

    /// Add a variable for every member of an array.
    pub(super) fn array_members(
        mut self,
        member_unit: &'a UnitInfo,
        type_offset: UnitOffset,
        array_key: ObjectRef,
        subranges: Vec<Range<u64>>,
    ) -> Result<(), DebugError> {
        self.jobs.push(Job::ExpandArray {
            member_unit,
            type_offset,
            array_key,
            subranges,
        });
        self.run()
    }

    /// Add the variable that `pointer` points at, and resolve its type.
    pub(super) fn pointer_target(
        mut self,
        unit: &'a UnitInfo,
        type_offset: UnitOffset,
        pointer: &Variable,
    ) -> Result<(), DebugError> {
        if !unit.points_at_an_object(self.debug_info, pointer, self.memory, unit, type_offset) {
            let pointer_key = pointer.variable_key;
            if let Some(mut pointer) = self.cache.get_variable_by_key(pointer_key) {
                pointer.variable_node_type = VariableNodeType::DoNotRecurse;
                self.cache.update_variable(&pointer)?;
            }
            return Ok(());
        }

        let pointer_key = pointer.variable_key;
        let mut target = self.cache.create_variable(pointer_key, Some(unit))?;
        target.name = target_name(&pointer.name);
        self.cache.update_variable(&target)?;

        self.jobs.push(Job::ExtractValue { key: pointer_key });
        self.jobs.push(Job::RemoveIfUnit {
            key: target.variable_key,
        });
        self.jobs.push(Job::ExtractType {
            unit,
            offset: type_offset,
            parent_key: pointer_key,
            child_key: target.variable_key,
        });
        self.run()?;
        self.skip_pointee_type_node(pointer_key)
    }

    /// A pointer already names the type that it points at. A child that only repeats that type is
    /// noise, so the members of the type move up to the pointer.
    fn skip_pointee_type_node(&mut self, pointer_key: ObjectRef) -> Result<(), DebugError> {
        let pointer = self.load(pointer_key)?;
        if matches!(&pointer.name, VariableName::Named(name) if name == "data_ptr") {
            return Ok(());
        }

        let children: Vec<_> = self.cache.get_children(pointer_key).cloned().collect();
        let [pointee] = children.as_slice() else {
            return Ok(());
        };
        if !matches!(
            pointee.type_name.inner(),
            VariableType::Struct(_) | VariableType::Enum(_)
        ) {
            return Ok(());
        }
        if !self.cache.has_children(pointee) {
            return Ok(());
        }

        self.cache.adopt_grand_children(&pointer, pointee)?;
        Ok(())
    }

    /// Add the members of a structured type now. The variable keeps its deferred node type, so
    /// that the cache does not expand it a second time.
    fn expand_now(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        let deferred = std::mem::replace(
            &mut child_variable.variable_node_type,
            VariableNodeType::RecurseToBaseType,
        );
        self.cache.update_variable(child_variable)?;

        let key = child_variable.variable_key;
        self.jobs.extend([
            Job::ExtractValue { key },
            Job::ProcessStruct { unit, offset, key },
            Job::RestoreNodeType {
                key,
                node_type: deferred,
            },
            Job::ProcessTree {
                unit,
                offset,
                parent_key: key,
            },
        ]);
        Ok(())
    }

    fn run(&mut self) -> Result<(), DebugError> {
        while let Some(job) = self.jobs.pop() {
            self.run_job(job)?;
        }
        Ok(())
    }

    fn load(&self, key: ObjectRef) -> Result<Variable, DebugError> {
        self.cache
            .get_variable_by_key(key)
            .ok_or_else(|| DebugError::Other(format!("Failed to find variable {key:?}.")))
    }

    /// Take the jobs scheduled since `mark`. The caller pushes the jobs that must run once that
    /// work is done, then pushes these back on top.
    fn scheduled_since(&mut self, mark: usize) -> Vec<Job<'a>> {
        self.jobs.split_off(mark)
    }

    fn run_job(&mut self, job: Job<'a>) -> Result<(), DebugError> {
        match job {
            Job::ProcessTree {
                unit,
                offset,
                parent_key,
            } => self.process_tree(unit, offset, parent_key),
            Job::FinishTree { unit, parent_key } => self.finish_tree(unit, parent_key),
            Job::ProcessVariable {
                unit,
                offset,
                parent_key,
            } => self.process_variable(unit, offset, parent_key),
            Job::ProcessVariableFinish {
                unit,
                offset,
                child_key,
            } => self.process_variable_finish(unit, offset, child_key),
            Job::ProcessNamespace {
                unit,
                offset,
                parent_key,
            } => self.process_namespace(unit, offset, parent_key),
            Job::DropEmptyNamespace(key) => self.drop_empty_namespace(key),
            Job::ProcessLexicalBlock {
                unit,
                offset,
                parent_key,
            } => self.process_lexical_block(unit, offset, parent_key),
            Job::ProcessVariantPart {
                unit,
                offset,
                parent_key,
            } => self.process_variant_part(unit, offset, parent_key),
            Job::ProcessVariantPartFinish {
                unit,
                offset,
                parent_key,
                dummy_key,
            } => self.process_variant_part_finish(unit, offset, parent_key, dummy_key),
            Job::ProcessVariantNode {
                unit,
                offset,
                parent_key,
            } => self.process_variant_node(unit, offset, parent_key),
            Job::ProcessVariant {
                unit,
                offset,
                parent_key,
                child_key,
            } => self.process_variant(unit, offset, parent_key, child_key),
            Job::ProcessVariantFinish {
                unit,
                offset,
                parent_key,
                child_key,
            } => self.process_variant_finish(unit, offset, parent_key, child_key),
            Job::AdoptGrandChildren {
                parent_key,
                child_key,
            } => self.adopt_grand_children(parent_key, child_key),
            Job::ResumeNodeAttributes(resume) => self.resume_node_attributes(resume),
            Job::ApplyDiscriminant {
                parent_key,
                discriminant_key,
            } => self.apply_discriminant(parent_key, discriminant_key),
            Job::ExtractType {
                unit,
                offset,
                parent_key,
                child_key,
            } => {
                let entry = unit.unit.entry(offset)?;
                let parent = self.load(parent_key)?;
                let mut child = self.load(child_key)?;
                self.extract_type(unit, &entry, &parent, &mut child)
            }
            Job::ExtractValue { key } => self.extract_value(key),
            Job::ApplyModifier { key, modifier } => self.apply_modifier(key, modifier),
            Job::RestoreNodeType { key, node_type } => self.restore_node_type(key, node_type),
            Job::ProcessStruct { unit, offset, key } => self.process_struct_job(unit, offset, key),
            Job::RemoveIfUnit { key } => self.remove_if_unit(key),
            Job::ExpandArray {
                member_unit,
                type_offset,
                array_key,
                subranges,
            } => self.expand_array(member_unit, type_offset, array_key, subranges),
            Job::ExpandArrayElement(expansion) => self.expand_array_element(expansion),
            Job::ExpandArrayAfterElement {
                expansion,
                member_key,
            } => self.expand_array_after_element(expansion, member_key),
            Job::VisitStatics {
                unit,
                offset,
                parent_key,
            } => self.visit_statics(unit, offset, parent_key),
            Job::VisitNamespace {
                unit,
                offset,
                parent_key,
            } => self.visit_namespace(unit, offset, parent_key),
        }
    }

    fn extract_value(&mut self, key: ObjectRef) -> Result<(), DebugError> {
        let mut variable = self.load(key)?;
        variable.extract_value(self.memory, self.cache);
        self.cache.update_variable(&variable)
    }

    fn apply_modifier(&mut self, key: ObjectRef, modifier: Modifier) -> Result<(), DebugError> {
        let mut child = self.load(key)?;
        self.apply_modifier_to(&mut child, modifier);
        self.cache.update_variable(&child)
    }

    fn apply_modifier_to(&self, child: &mut Variable, modifier: Modifier) {
        if matches!(modifier, Modifier::Typedef(_)) && child.variable_node_type.is_deferred() {
            // Read the value again through the resolved type information.
            child.value = VariableValue::Empty;
        }
        child.type_name = VariableType::Modified(
            modifier,
            Box::new(std::mem::replace(
                &mut child.type_name,
                VariableType::Unknown,
            )),
        );
    }

    fn restore_node_type(
        &mut self,
        key: ObjectRef,
        node_type: VariableNodeType,
    ) -> Result<(), DebugError> {
        let mut variable = self.load(key)?;
        variable.variable_node_type = node_type;
        self.cache.update_variable(&variable)
    }

    fn process_struct_job(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        key: ObjectRef,
    ) -> Result<(), DebugError> {
        let entry = unit.unit.entry(offset)?;
        let mut variable = self.load(key)?;
        unit.process_struct(
            self.debug_info,
            &entry,
            &mut variable,
            self.memory,
            self.cache,
            self.frame_info,
        )?;
        self.cache.update_variable(&variable)
    }

    fn remove_if_unit(&mut self, key: ObjectRef) -> Result<(), DebugError> {
        let Some(variable) = self.cache.get_variable_by_key(key) else {
            return Ok(());
        };
        if matches!(variable.type_name.inner(), VariableType::Base(name) if name == "()") {
            self.cache.remove_cache_entry(key)?;
        }
        Ok(())
    }

    fn apply_discriminant(
        &mut self,
        parent_key: ObjectRef,
        discriminant_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let mut parent = self.load(parent_key)?;
        let discriminant = self.load(discriminant_key)?;
        parent.role =
            VariantRole::VariantPart(discriminant.integer_value(self.memory).unwrap_or(u64::MAX));
        self.cache.remove_cache_entry(discriminant_key)?;
        self.cache.update_variable(&parent)
    }

    fn process_tree(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let parent = self.load(parent_key)?;
        if !parent.is_valid() {
            self.cache.update_variable(&parent)?;
            return Ok(());
        }

        tracing::trace!("process_tree for parent {:?}", parent.variable_key);

        let child_dies = child_offsets(unit, offset)?;
        self.jobs.push(Job::FinishTree { unit, parent_key });
        for (child_offset, tag) in child_dies.into_iter().rev() {
            match tag {
                gimli::DW_TAG_namespace => {
                    self.jobs.push(Job::ProcessNamespace {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_formal_parameter | gimli::DW_TAG_variable | gimli::DW_TAG_member => {
                    self.jobs.push(Job::ProcessVariable {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_variant_part => {
                    self.jobs.push(Job::ProcessVariantPart {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_variant => {
                    self.jobs.push(Job::ProcessVariantNode {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_lexical_block => {
                    self.jobs.push(Job::ProcessLexicalBlock {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_template_type_parameter => {
                    // The parent node for Rust generic type parameter
                    // These show up as a child of structures they belong to and points to the type that matches the
                    // template.
                    // They are followed by a sibling of `DW_TAG_member` with name '__0' that has all the attributes
                    // needed to resolve the value.
                    // TODO: If there are multiple types supported, then I suspect there will be additional
                    // `DW_TAG_member` siblings. We will need to match those correctly.
                }
                gimli::DW_TAG_inlined_subroutine
                | gimli::DW_TAG_base_type
                | gimli::DW_TAG_pointer_type
                | gimli::DW_TAG_structure_type
                | gimli::DW_TAG_enumeration_type
                | gimli::DW_TAG_array_type
                | gimli::DW_TAG_subroutine_type
                | gimli::DW_TAG_subprogram
                | gimli::DW_TAG_union_type
                | gimli::DW_TAG_typedef
                | gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type => {}
                unimplemented => {
                    tracing::debug!(
                        "Unimplemented: Encountered unimplemented DwTag {:?} for Variable {:?}",
                        unimplemented.static_string(),
                        parent.name
                    )
                }
            }
        }

        Ok(())
    }

    fn finish_tree(&mut self, unit: &'a UnitInfo, parent_key: ObjectRef) -> Result<(), DebugError> {
        let parent = self.load(parent_key)?;
        if let Some(offset) = self.default_variants.remove(&parent_key)
            && !self.cache.has_children(&parent)
        {
            let child = self.cache.create_variable(parent_key, Some(unit))?;
            self.jobs.push(Job::ExtractValue { key: parent_key });
            self.jobs.push(Job::ProcessVariant {
                unit,
                offset,
                parent_key,
                child_key: child.variable_key,
            });
            return Ok(());
        }

        self.extract_value(parent_key)
    }

    fn process_variable(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let mut parent = self.load(parent_key)?;
        let mut child = self.cache.create_variable(parent_key, Some(unit))?;
        self.jobs.push(Job::ProcessVariableFinish {
            unit,
            offset,
            child_key: child.variable_key,
        });
        self.start_node_attributes(unit, offset, &mut parent, &mut child)?;
        self.cache.update_variable(&parent)
    }

    fn process_variable_finish(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        child_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let Some(child) = self.cache.get_variable_by_key(child_key) else {
            return Ok(());
        };
        let die = unit.unit.entry(offset)?;
        let is_declaration =
            if let Some(AttributeValue::Flag(value)) = die.attr_value(gimli::DW_AT_declaration) {
                value
            } else {
                false
            };
        if is_declaration
            || child.type_name.is_phantom_data()
            || child.name == VariableName::Artificial
        {
            self.cache.remove_cache_entry(child.variable_key)?;
        } else if child.is_valid() {
            self.jobs.push(Job::ProcessTree {
                unit,
                offset,
                parent_key: child.variable_key,
            });
        }
        Ok(())
    }

    fn process_namespace(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let namespace_key =
            unit.ensure_namespace(self.debug_info, self.cache, parent_key, offset)?;
        self.jobs.push(Job::DropEmptyNamespace(namespace_key));
        self.jobs.push(Job::ProcessTree {
            unit,
            offset,
            parent_key: namespace_key,
        });
        Ok(())
    }

    fn drop_empty_namespace(&mut self, key: ObjectRef) -> Result<(), DebugError> {
        if let Some(namespace) = self.cache.get_variable_by_key(key)
            && !self.cache.has_children(&namespace)
        {
            self.cache.remove_cache_entry(key)?;
        }
        Ok(())
    }

    fn process_lexical_block(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let Some(program_counter) = self
            .frame_info
            .registers
            .get_program_counter()
            .and_then(|reg| reg.value)
        else {
            return Err(DebugError::WarnAndContinue {
                message: "Cannot unwind `Variable` without a valid PC (program_counter)"
                    .to_string(),
            });
        };
        let program_counter = program_counter.try_into()?;
        let die = unit.unit.entry(offset)?;
        let in_scope = die_contains_pc(self.debug_info, &unit.unit, &die, program_counter)?;

        if in_scope {
            // This is IN scope.
            // Recursively process each child, but pass the parent_variable, so that we don't create
            // intermediate nodes for scope identifiers.
            self.jobs.push(Job::ProcessTree {
                unit,
                offset,
                parent_key,
            });
        }
        Ok(())
    }

    fn process_variant_part(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let mut parent = self.load(parent_key)?;
        let mut child = self.cache.create_variable(parent_key, Some(unit))?;
        parent.role = VariantRole::VariantPart(u64::MAX);
        self.jobs.push(Job::ProcessVariantPartFinish {
            unit,
            offset,
            parent_key,
            dummy_key: child.variable_key,
        });
        self.start_node_attributes(unit, offset, &mut parent, &mut child)?;
        self.cache.update_variable(&parent)
    }

    fn process_variant_part_finish(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        dummy_key: ObjectRef,
    ) -> Result<(), DebugError> {
        if self.cache.get_variable_by_key(dummy_key).is_some() {
            self.cache.remove_cache_entry(dummy_key)?;
        }
        self.jobs.push(Job::ProcessTree {
            unit,
            offset,
            parent_key,
        });
        Ok(())
    }

    fn process_variant_node(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let parent = self.load(parent_key)?;
        if self.cache.has_children(&parent) {
            return Ok(());
        }

        let mut child = self.cache.create_variable(parent_key, Some(unit))?;
        let mut tree = unit.unit.entries_tree(Some(offset))?;
        unit.extract_variant_discriminant(&tree.root()?, &mut child)?;
        self.cache.update_variable(&child)?;

        if let VariantRole::Variant(discriminant) = child.role {
            if parent.role == VariantRole::VariantPart(discriminant) {
                self.jobs.push(Job::ProcessVariant {
                    unit,
                    offset,
                    parent_key,
                    child_key: child.variable_key,
                });
                return Ok(());
            }

            if discriminant == u64::MAX {
                self.default_variants.insert(parent_key, offset);
            }
        }

        self.cache.remove_cache_entry(child.variable_key)?;
        Ok(())
    }

    fn process_variant(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let mut parent = self.load(parent_key)?;
        let mut child = self.load(child_key)?;
        self.jobs.push(Job::ProcessVariantFinish {
            unit,
            offset,
            parent_key,
            child_key,
        });
        self.start_node_attributes(unit, offset, &mut parent, &mut child)?;
        self.cache.update_variable(&parent)
    }

    fn process_variant_finish(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let Some(mut child) = self.cache.get_variable_by_key(child_key) else {
            return Ok(());
        };
        if !child.is_valid() {
            self.cache.remove_cache_entry(child_key)?;
            return Ok(());
        }

        let parent = self.load(parent_key)?;
        let die = unit.unit.entry(offset)?;
        unit.process_memory_location(
            self.debug_info,
            &die,
            &parent,
            &mut child,
            self.memory,
            self.frame_info,
        )?;
        self.cache.update_variable(&child)?;
        self.jobs.push(Job::AdoptGrandChildren {
            parent_key,
            child_key,
        });
        self.jobs.push(Job::ProcessTree {
            unit,
            offset,
            parent_key: child_key,
        });
        Ok(())
    }

    fn adopt_grand_children(
        &mut self,
        parent_key: ObjectRef,
        child_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let Some(parent) = self.cache.get_variable_by_key(parent_key) else {
            return Ok(());
        };
        let Some(child) = self.cache.get_variable_by_key(child_key) else {
            return Ok(());
        };
        if child.is_valid() {
            self.cache.adopt_grand_children(&parent, &child)?;
        }
        Ok(())
    }

    fn resume_node_attributes(&mut self, resume: AttrResume<'a>) -> Result<(), DebugError> {
        let mut parent = self.load(resume.parent_key)?;
        let mut child = self.load(resume.child_key)?;
        self.process_node_attributes(resume, false, &mut parent, &mut child)?;
        self.cache.update_variable(&parent)
    }

    fn start_node_attributes(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent: &mut Variable,
        child: &mut Variable,
    ) -> Result<(), DebugError> {
        self.process_node_attributes(
            AttrResume {
                unit,
                die_offset: offset,
                attributes_offset: None,
                parent_key: parent.variable_key,
                child_key: child.variable_key,
                skip: 0,
            },
            true,
            parent,
            child,
        )
    }

    fn process_node_attributes(
        &mut self,
        resume: AttrResume<'a>,
        do_header: bool,
        parent_variable: &mut Variable,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        let unit = resume.unit;
        let skip = resume.skip;
        let tree_node = unit.unit.entry(resume.die_offset)?;
        let attributes_offset = if do_header {
            child_variable.parent_key = parent_variable.variable_key;

            let abstract_origin = match unit.attributes_entry(
                gimli::DW_AT_abstract_origin,
                self.debug_info,
                &tree_node,
                parent_variable,
                child_variable,
                self.memory,
                self.frame_info,
            )? {
                AttributesEntry::Found(entry) => Some(entry),
                AttributesEntry::Unsupported => None,
                AttributesEntry::NotFound => Some(tree_node.clone()),
            };

            let attributes_entry = match unit.attributes_entry(
                gimli::DW_AT_specification,
                self.debug_info,
                &tree_node,
                parent_variable,
                child_variable,
                self.memory,
                self.frame_info,
            )? {
                AttributesEntry::Found(entry) => Some(entry),
                AttributesEntry::Unsupported => None,
                AttributesEntry::NotFound => abstract_origin,
            };

            if let Some(entry) = attributes_entry.as_ref()
                && let Ok(Some(name)) = extract_name(self.debug_info, &unit.unit, entry)
            {
                child_variable.name = VariableName::Named(name);
            }

            if let Some(entry) = attributes_entry.as_ref() {
                child_variable.source_location =
                    unit.extract_source_location(self.debug_info, entry)?;
                Some(entry.offset())
            } else {
                None
            }
        } else {
            resume.attributes_offset
        };

        if let Some(attributes_offset) = attributes_offset {
            let attributes_entry = unit.unit.entry(attributes_offset)?;
            let mut idx = 0;
            for attr in attributes_entry.attrs() {
                if idx < skip {
                    idx += 1;
                    continue;
                }
                let next_skip = idx + 1;
                idx += 1;

                match attr.name() {
                    gimli::DW_AT_location | gimli::DW_AT_data_member_location => {
                        // The child_variable.location is calculated with attribute gimli::DW_AT_type, to ensure it
                        // gets done before DW_AT_type is processed
                    }
                    gimli::DW_AT_name => {
                        // This was done before we started looping through attributes, so we can ignore it.
                    }
                    gimli::DW_AT_decl_file | gimli::DW_AT_decl_line | gimli::DW_AT_decl_column => {
                        // Handled in extract_source_location()
                    }
                    gimli::DW_AT_containing_type => {
                        // TODO: Implement [documented RUST extensions to DWARF standard](https://rustc-dev-guide.rust-lang.org/debugging-support-in-rustc.html?highlight=dwarf#dwarf-and-rustc)
                    }
                    gimli::DW_AT_type => {
                        // The rules to calculate the type of a child variable are complex, and depend on a number of
                        // other attributes.
                        // Depending on the presence and value of these attributes, the [Variable::memory_location] may
                        // need to be calculated differently.
                        // - The `DW_AT_type` of the parent (e.g. is it a pointer, or a struct, or an array, etc.).
                        // - The `DW_AT_address_class of the child (we need to know if it is present, and if it has a
                        //   value of 0 - unspecified)
                        // - The `DW_AT_data_member_location` of the child.
                        // - The `DW_AT_location` of the child.
                        // - The `DW_AT_byte_size` of the child.
                        // - The `DW_AT_name` of the data type node.
                        let mark = self.jobs.len();
                        self.process_type_attribute(
                            unit,
                            attr,
                            &attributes_entry,
                            parent_variable,
                            child_variable,
                        )?;
                        let scheduled = self.scheduled_since(mark);
                        if !scheduled.is_empty() {
                            self.jobs.push(Job::ResumeNodeAttributes(AttrResume {
                                unit,
                                die_offset: tree_node.offset(),
                                attributes_offset: Some(attributes_offset),
                                parent_key: parent_variable.variable_key,
                                child_key: child_variable.variable_key,
                                skip: next_skip,
                            }));
                            self.jobs.extend(scheduled);
                            self.cache.update_variable(child_variable)?;
                            return Ok(());
                        }
                    }
                    gimli::DW_AT_enum_class => {
                        let value = match attr.value() {
                            AttributeValue::Flag(true) => {
                                VariableValue::Valid(child_variable.compact_type_name())
                            }
                            AttributeValue::Flag(false) => VariableValue::Error(
                                "Unimplemented: DW_AT_enum_class(false)".to_string(),
                            ),
                            other_attribute_value => VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_enum_class: {other_attribute_value:?}"
                            )),
                        };

                        child_variable.set_value(value);
                    }
                    gimli::DW_AT_const_value => {
                        let attr_value = attr.value();
                        let variable_value = if let Some(const_value) = attr_value.udata_value() {
                            VariableValue::Valid(const_value.to_string())
                        } else if let Some(const_value) = attr_value.sdata_value() {
                            VariableValue::Valid(const_value.to_string())
                        } else {
                            VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_const_value: {attr_value:?}"
                            ))
                        };

                        child_variable.set_value(variable_value)
                    }
                    gimli::DW_AT_alignment => {
                        // TODO: Figure out when (if at all) we need to do anything with DW_AT_alignment for the
                        // purposes of decoding data values.
                    }
                    gimli::DW_AT_artificial => {
                        // These are references for entries like discriminant values of `VariantParts`.
                        child_variable.name = VariableName::Artificial;
                    }
                    gimli::DW_AT_discr => {
                        let mark = self.jobs.len();
                        self.process_discriminant(unit, parent_variable, child_variable, attr)?;
                        let scheduled = self.scheduled_since(mark);
                        if !scheduled.is_empty() {
                            self.jobs.push(Job::ResumeNodeAttributes(AttrResume {
                                unit,
                                die_offset: tree_node.offset(),
                                attributes_offset: Some(attributes_offset),
                                parent_key: parent_variable.variable_key,
                                child_key: child_variable.variable_key,
                                skip: next_skip,
                            }));
                            self.jobs.extend(scheduled);
                            self.cache.update_variable(child_variable)?;
                            self.cache.update_variable(parent_variable)?;
                            return Ok(());
                        }
                    }
                    gimli::DW_AT_linkage_name => {
                        let value = attr.value();
                        let raw_str = self.debug_info.dwarf.attr_string(&unit.unit, value).ok();

                        let linkage_name = raw_str.and_then(|r| String::from_utf8(r.to_vec()).ok());

                        child_variable.linkage_name = linkage_name;
                    }
                    gimli::DW_AT_accessibility => {
                        // Silently ignore these for now.
                        // TODO: Add flag for public/private/protected for `Variable`, once we have a use case.
                    }
                    gimli::DW_AT_external => {
                        // TODO: Implement globally visible variables.
                    }
                    gimli::DW_AT_declaration => {
                        // Unimplemented.
                    }
                    gimli::DW_AT_encoding => {
                        // Ignore these. RUST data types handle this intrinsically.
                    }
                    gimli::DW_AT_discr_value => {
                        // Processed by `extract_variant_discriminant()`.
                    }
                    gimli::DW_AT_byte_size => {
                        // Processed by `extract_byte_size()`.
                    }
                    gimli::DW_AT_abstract_origin => {
                        // Processed before looping through all attributes
                    }
                    gimli::DW_AT_address_class => {
                        // Processed by `extract_type()`
                    }
                    gimli::DW_AT_data_bit_offset
                    | gimli::DW_AT_bit_offset
                    | gimli::DW_AT_bit_size => {
                        // Processed by `extract_bitfield_info()`
                    }
                    gimli::DW_AT_start_scope => {
                        // Processed by `apply_start_scope()`.
                    }
                    other_attribute => {
                        tracing::info!(
                            "Unimplemented: Variable Attribute {:.100} : {:.100}, with children = {}",
                            format!("{:?}", other_attribute.static_string()),
                            format!("{:?}", attributes_entry.attr_value(other_attribute)),
                            attributes_entry.has_children()
                        );
                    }
                }
            }
        }

        let attributes_entry = attributes_offset
            .map(|offset| unit.unit.entry(offset))
            .transpose()?;

        unit.process_bitfield_info(child_variable, &tree_node, self.cache)?;

        unit.apply_start_scope(
            self.debug_info,
            &tree_node,
            attributes_entry.as_ref(),
            child_variable,
            self.frame_info,
        )?;

        child_variable.extract_value(self.memory, self.cache);
        self.cache.update_variable(child_variable)?;

        Ok(())
    }

    fn process_discriminant(
        &mut self,
        unit: &'a UnitInfo,
        parent_variable: &mut Variable,
        child_variable: &mut Variable,
        attr: &gimli::Attribute<GimliReader>,
    ) -> Result<(), DebugError> {
        match attr.value() {
            AttributeValue::UnitRef(unit_ref) => {
                let mut discriminant_variable = self
                    .cache
                    .create_variable(parent_variable.variable_key, Some(unit))?;
                let mark = self.jobs.len();
                self.start_node_attributes(
                    unit,
                    unit_ref,
                    parent_variable,
                    &mut discriminant_variable,
                )?;
                let scheduled = self.scheduled_since(mark);
                self.jobs.push(Job::ApplyDiscriminant {
                    parent_key: parent_variable.variable_key,
                    discriminant_key: discriminant_variable.variable_key,
                });
                self.jobs.extend(scheduled);
            }
            other_attribute_value => {
                child_variable.set_value(VariableValue::Error(format!(
                    "Unimplemented: Attribute Value for DW_AT_discr {other_attribute_value:?}"
                )));
            }
        }
        Ok(())
    }

    fn process_type_attribute(
        &mut self,
        unit: &'a UnitInfo,
        attr: &gimli::Attribute<GimliReader>,
        attributes_entry: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        unit.process_memory_location(
            self.debug_info,
            attributes_entry,
            parent_variable,
            child_variable,
            self.memory,
            self.frame_info,
        )?;

        match self.debug_info.resolve_die_reference_with_unit(attr, unit) {
            Ok((unit_info, referenced_type_tree_node)) => self.extract_type(
                unit_info,
                &referenced_type_tree_node,
                parent_variable,
                child_variable,
            ),
            Err(error) => {
                child_variable.set_value(VariableValue::Error(format!(
                    "Failed to process DW_AT_type: {error:?}"
                )));
                Ok(())
            }
        }
    }

    /// Compute the type (base to complex) of a variable. Only base types have values.
    /// Complex types are references to node trees, that require traversal in similar ways to other DIE's like functions.
    /// This means [`extract_type()`][e] will schedule [`process_tree()`][p] jobs to build an integrated
    /// `tree` of variables with types and values.
    ///
    /// [e]: Self::extract_type()
    /// [p]: Self::process_tree()
    fn extract_type(
        &mut self,
        unit: &'a UnitInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        let type_name = match unit.extract_type_name(self.debug_info, node) {
            Ok(name) => name,
            Err(error) => {
                let message = format!("Error: evaluating type name: {error:?}");
                child_variable.set_value(VariableValue::Error(message.clone()));
                Some(message)
            }
        };

        if !child_variable.is_valid() {
            self.cache.update_variable(child_variable)?;
            return Ok(());
        }
        child_variable.type_node_offset = node.offset().to_debug_info_offset(&unit.unit.header);

        let mark = self.jobs.len();
        match node.tag() {
            gimli::DW_TAG_base_type => {
                child_variable.type_name = VariableType::Base(
                    type_name.unwrap_or_else(|| "<unnamed base type>".to_string()),
                );
                unit.process_memory_location(
                    self.debug_info,
                    node,
                    parent_variable,
                    child_variable,
                    self.memory,
                    self.frame_info,
                )?;
            }
            gimli::DW_TAG_pointer_type => {
                self.extract_pointer_type(unit, node, parent_variable, child_variable, type_name)?
            }
            gimli::DW_TAG_structure_type => {
                self.extract_struct(unit, type_name, node, parent_variable, child_variable)?
            }
            gimli::DW_TAG_enumeration_type => {
                unit.extract_enumeration_type(
                    child_variable,
                    type_name,
                    self.debug_info,
                    node,
                    parent_variable,
                    self.memory,
                    self.frame_info,
                )?;
            }
            gimli::DW_TAG_array_type => {
                self.extract_array_type(unit, node, parent_variable, child_variable)?
            }
            gimli::DW_TAG_union_type => {
                child_variable.type_name =
                    VariableType::Base(type_name.unwrap_or_else(|| "<unnamed union>".to_string()));
                unit.process_memory_location(
                    self.debug_info,
                    node,
                    parent_variable,
                    child_variable,
                    self.memory,
                    self.frame_info,
                )?;

                if child_variable.memory_location.holds_value() {
                    child_variable.variable_node_type =
                        VariableNodeType::TypeOffset(unit.debug_info_offset()?, node.offset());
                    let width = expansion_width(unit, node)?;
                    if width > 0 && self.budget.affords(width) {
                        self.expand_now(unit, node.offset(), child_variable)?;
                    }
                } else {
                    child_variable.variable_node_type = VariableNodeType::DoNotRecurse;
                }
            }
            gimli::DW_TAG_subroutine_type => {
                self.extract_subroutine_type(unit, node, child_variable)?
            }
            other @ (gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type) => self.extract_modified_type(
                unit,
                node,
                parent_variable,
                child_variable,
                type_name,
                other,
            )?,
            other => {
                child_variable.set_value(VariableValue::Error(format!(
                    "<unimplemented: type: {other}>"
                )));
                child_variable.type_name = VariableType::Other("unimplemented".to_string());
                self.cache
                    .remove_cache_entry_children(child_variable.variable_key)?;
            }
        }

        // A scheduled job owns the value of the variable, and settles it when the type is complete.
        if self.jobs.len() == mark {
            child_variable.extract_value(self.memory, self.cache);
            self.cache.update_variable(child_variable)?;
        }

        Ok(())
    }

    fn extract_pointer_type(
        &mut self,
        unit: &'a UnitInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
        type_name: Option<String>,
    ) -> Result<(), DebugError> {
        child_variable.type_name = unit.pointer_from_name(type_name);
        unit.process_memory_location(
            self.debug_info,
            node,
            parent_variable,
            child_variable,
            self.memory,
            self.frame_info,
        )?;

        match node.attr(gimli::DW_AT_type) {
            Some(attr) => {
                if !self.cache.has_children(child_variable) {
                    match self.debug_info.resolve_die_reference_with_unit(attr, unit) {
                        Ok((referenced_unit, referenced_node)) => {
                            if referenced_node.tag() != gimli::DW_TAG_subroutine_type
                                && unit.points_at_an_object(
                                    self.debug_info,
                                    child_variable,
                                    self.memory,
                                    referenced_unit,
                                    referenced_node.offset(),
                                )
                            {
                                child_variable.variable_node_type = VariableNodeType::PointerTarget(
                                    referenced_unit.debug_info_offset()?,
                                    referenced_node.offset(),
                                );
                            }
                        }
                        Err(error) => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Failed to process DW_AT_type: {error:?}"
                            )));
                        }
                    }
                }
            }
            None => {
                child_variable.set_value(
                    unit.language
                        .process_tag_with_no_type(child_variable, gimli::DW_TAG_pointer_type),
                );
            }
        }

        Ok(())
    }

    fn extract_struct(
        &mut self,
        unit: &'a UnitInfo,
        type_name: Option<String>,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        let type_name = type_name.unwrap_or_else(|| "<unnamed struct>".to_string());
        let mut visiting = HashSet::new();
        if let Some(offset) = node.offset().to_debug_info_offset(&unit.unit.header) {
            visiting.insert(offset);
        }
        let named = unit.extract_named_type(self.debug_info, node, type_name, &mut visiting);
        child_variable.type_name = VariableType::Struct(named);
        unit.process_memory_location(
            self.debug_info,
            node,
            parent_variable,
            child_variable,
            self.memory,
            self.frame_info,
        )?;

        if child_variable.memory_location.holds_value() {
            let width = expansion_width(unit, node)?;
            if width > 0 {
                child_variable.variable_node_type =
                    VariableNodeType::TypeOffset(unit.debug_info_offset()?, node.offset());
                if !unit.language.is_side_effecting(&child_variable.type_name)
                    && (unit
                        .language
                        .auto_resolve_children(&child_variable.type_name)
                        || self.budget.affords(width))
                {
                    return self.expand_now(unit, node.offset(), child_variable);
                }
            } else {
                child_variable.variable_node_type = VariableNodeType::DoNotRecurse;
                child_variable.set_value(VariableValue::Valid(child_variable.compact_type_name()));
            }
        } else {
            child_variable.variable_node_type = VariableNodeType::DoNotRecurse;
            child_variable.set_value(VariableValue::Valid(format!(
                "{} @ {}",
                child_variable.compact_type_name(),
                child_variable.memory_location
            )));
        }

        unit.process_struct(
            self.debug_info,
            node,
            child_variable,
            self.memory,
            self.cache,
            self.frame_info,
        )?;
        Ok(())
    }

    fn extract_array_type(
        &mut self,
        unit: &'a UnitInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        let subranges = match unit.extract_array_range(node.offset()) {
            Ok(subranges) => subranges,
            Err(error) => {
                child_variable.set_value(VariableValue::Error(format!(
                    "Error: Failed to extract array range: {error:?}"
                )));
                return Ok(());
            }
        };

        match node.attr(gimli::DW_AT_type) {
            Some(attr) => {
                unit.process_memory_location(
                    self.debug_info,
                    node,
                    parent_variable,
                    child_variable,
                    self.memory,
                    self.frame_info,
                )?;

                match self.debug_info.resolve_die_reference_with_unit(attr, unit) {
                    Ok((member_unit, array_member_type_node)) => {
                        self.cache.update_variable(child_variable)?;
                        self.jobs.push(Job::ExtractValue {
                            key: child_variable.variable_key,
                        });
                        self.jobs.push(Job::ExpandArray {
                            member_unit,
                            type_offset: array_member_type_node.offset(),
                            array_key: child_variable.variable_key,
                            subranges,
                        });
                        return Ok(());
                    }
                    Err(error) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Failed to process DW_AT_type: {error:?}"
                        )));
                    }
                }
            }
            None => {
                child_variable.set_value(
                    unit.language
                        .process_tag_with_no_type(child_variable, gimli::DW_TAG_array_type),
                );
            }
        }

        Ok(())
    }

    fn extract_subroutine_type(
        &mut self,
        unit: &UnitInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        child_variable: &mut Variable,
    ) -> Result<(), DebugError> {
        match node.attr(gimli::DW_AT_type) {
            Some(data_type_attribute) => {
                match self
                    .debug_info
                    .resolve_die_reference_with_unit(data_type_attribute, unit)
                {
                    Ok((subroutine_unit, subroutine_type_node)) => {
                        child_variable.type_name = match extract_name(
                            self.debug_info,
                            &subroutine_unit.unit,
                            &subroutine_type_node,
                        ) {
                            Ok(Some(name_attr)) => VariableType::Other(name_attr),
                            Ok(None) => VariableType::Unknown,
                            Err(error) => VariableType::Other(format!(
                                "Error: evaluating subroutine type name: {error:?} "
                            )),
                        };
                    }
                    Err(error) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Failed to process DW_AT_type: {error:?}"
                        )));
                    }
                }
            }
            None => {
                child_variable.set_value(VariableValue::Valid("<No Return Value>".to_string()));
                child_variable.type_name = VariableType::Unknown;
            }
        }
        Ok(())
    }

    fn extract_modified_type(
        &mut self,
        unit: &'a UnitInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
        type_name: Option<String>,
        other: gimli::DwTag,
    ) -> Result<(), DebugError> {
        match node.attr(gimli::DW_AT_type) {
            Some(attr) => {
                let modifier = match other {
                    gimli::DW_TAG_typedef => Modifier::Typedef(
                        type_name.unwrap_or_else(|| "<unnamed typedef>".to_string()),
                    ),
                    gimli::DW_TAG_const_type => Modifier::Const,
                    gimli::DW_TAG_volatile_type => Modifier::Volatile,
                    gimli::DW_TAG_restrict_type => Modifier::Restrict,
                    gimli::DW_TAG_atomic_type => Modifier::Atomic,
                    _ => unreachable!(),
                };

                let mark = self.jobs.len();
                self.process_type_attribute(unit, attr, node, parent_variable, child_variable)?;
                let scheduled = self.scheduled_since(mark);
                if scheduled.is_empty() {
                    self.apply_modifier_to(child_variable, modifier);
                } else {
                    self.jobs.push(Job::ExtractValue {
                        key: child_variable.variable_key,
                    });
                    self.jobs.push(Job::ApplyModifier {
                        key: child_variable.variable_key,
                        modifier,
                    });
                    self.jobs.extend(scheduled);
                    self.cache.update_variable(child_variable)?;
                }
            }
            None => {
                child_variable.set_value(
                    unit.language
                        .process_tag_with_no_type(child_variable, other),
                );
            }
        }
        Ok(())
    }

    fn expand_array(
        &mut self,
        member_unit: &'a UnitInfo,
        type_offset: UnitOffset,
        array_key: ObjectRef,
        subranges: Vec<Range<u64>>,
    ) -> Result<(), DebugError> {
        let mut array_variable = self.load(array_key)?;
        let Some((current_range, remaining_ranges)) = subranges.split_first() else {
            array_variable.set_value(VariableValue::Error(
                "Error processing range for array, unexpected empty range. \
                    This is a known issue, see https://github.com/probe-rs/probe-rs/issues/2687"
                    .to_string(),
            ));
            self.cache.update_variable(&array_variable)?;
            return Ok(());
        };

        let explode_range = if current_range.is_empty() {
            0..1
        } else {
            current_range.clone()
        };

        self.jobs.push(Job::ExpandArrayElement(ArrayExpansion {
            member_unit,
            type_offset,
            array_key,
            member_index: explode_range.start,
            explode_start: explode_range.start,
            explode_end: explode_range.end,
            current_range_empty: current_range.is_empty(),
            remaining_ranges: remaining_ranges.to_vec(),
        }));
        Ok(())
    }

    fn expand_array_element(&mut self, expansion: ArrayExpansion<'a>) -> Result<(), DebugError> {
        let ArrayExpansion {
            member_unit,
            type_offset,
            array_key,
            member_index,
            explode_start,
            explode_end,
            current_range_empty,
            remaining_ranges,
        } = expansion;
        let array_variable = self.load(array_key)?;
        let mut array_member_variable = self.cache.create_variable(array_key, Some(member_unit))?;
        array_member_variable.name = VariableName::Indexed(member_index);
        array_member_variable.source_location = array_variable.source_location.clone();

        let array_member_type_node = member_unit.unit.entry(type_offset)?;
        member_unit.process_memory_location(
            self.debug_info,
            &array_member_type_node,
            &array_variable,
            &mut array_member_variable,
            self.memory,
            self.frame_info,
        )?;
        self.cache.update_variable(&array_member_variable)?;

        self.jobs.push(Job::ExpandArrayAfterElement {
            expansion: ArrayExpansion {
                member_unit,
                type_offset,
                array_key,
                member_index,
                explode_start,
                explode_end,
                current_range_empty,
                remaining_ranges: remaining_ranges.clone(),
            },
            member_key: array_member_variable.variable_key,
        });

        if !remaining_ranges.is_empty() {
            self.jobs.push(Job::ExpandArray {
                member_unit,
                type_offset,
                array_key: array_member_variable.variable_key,
                subranges: remaining_ranges,
            });
        } else {
            self.jobs.push(Job::ExtractType {
                unit: member_unit,
                offset: type_offset,
                parent_key: array_key,
                child_key: array_member_variable.variable_key,
            });
        }

        Ok(())
    }

    fn expand_array_after_element(
        &mut self,
        expansion: ArrayExpansion<'a>,
        member_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let ArrayExpansion {
            member_unit,
            type_offset,
            array_key,
            member_index,
            explode_start,
            explode_end,
            current_range_empty,
            remaining_ranges,
        } = expansion;
        let mut array_variable = self.load(array_key)?;
        let mut array_member_variable = self.load(member_key)?;

        if member_index == explode_start {
            let item_count = if current_range_empty {
                0
            } else {
                (explode_end - explode_start) as usize
            };

            array_variable.type_name = VariableType::Array {
                count: item_count,
                item_type_name: Box::new(array_member_variable.type_name.clone()),
            };
            if let Some(item_byte_size) = array_member_variable.byte_size {
                array_variable.byte_size = Some(item_byte_size * item_count as u64);
            }
            self.cache.update_variable(&array_variable)?;
        }

        array_member_variable.extract_value(self.memory, self.cache);
        self.cache.update_variable(&array_member_variable)?;

        if member_index + 1 < explode_end {
            self.jobs.push(Job::ExpandArrayElement(ArrayExpansion {
                member_unit,
                type_offset,
                array_key,
                member_index: member_index + 1,
                explode_start,
                explode_end,
                current_range_empty,
                remaining_ranges,
            }));
        } else if current_range_empty {
            self.cache
                .remove_cache_entry_children(array_variable.variable_key)?;
        }

        Ok(())
    }

    fn visit_statics(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let child_dies = child_offsets(unit, offset)?;
        for (child_offset, tag) in child_dies.into_iter().rev() {
            match tag {
                gimli::DW_TAG_namespace => {
                    self.jobs.push(Job::VisitNamespace {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                gimli::DW_TAG_formal_parameter | gimli::DW_TAG_variable | gimli::DW_TAG_member => {
                    self.jobs.push(Job::ProcessVariable {
                        unit,
                        offset: child_offset,
                        parent_key,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn visit_namespace(
        &mut self,
        unit: &'a UnitInfo,
        offset: UnitOffset,
        parent_key: ObjectRef,
    ) -> Result<(), DebugError> {
        let namespace_key =
            unit.ensure_namespace(self.debug_info, self.cache, parent_key, offset)?;
        self.jobs.push(Job::DropEmptyNamespace(namespace_key));
        self.jobs.push(Job::VisitStatics {
            unit,
            offset,
            parent_key: namespace_key,
        });
        Ok(())
    }
}

fn child_offsets(
    unit: &UnitInfo,
    offset: UnitOffset,
) -> Result<Vec<(UnitOffset, gimli::DwTag)>, DebugError> {
    let mut child_dies = Vec::new();
    let mut tree = unit.unit.entries_tree(Some(offset))?;
    let mut children = tree.root()?.children();
    while let Some(child) = children.next()? {
        child_dies.push((child.entry().offset(), child.entry().tag()));
    }
    Ok(child_dies)
}

#[cfg(test)]
mod test {
    use super::{Budget, NARROW_MEMBERS};

    #[test]
    fn a_narrow_type_expands_after_the_wide_budget_runs_out() {
        let mut budget = Budget {
            wide: 1,
            total: 1000,
        };

        assert!(budget.affords(NARROW_MEMBERS + 1));
        assert!(!budget.affords(NARROW_MEMBERS + 1));

        for _ in 0..100 {
            assert!(budget.affords(NARROW_MEMBERS));
        }
    }

    #[test]
    fn a_wide_type_expands_while_the_wide_budget_lasts() {
        let mut budget = Budget {
            wide: 3,
            total: 1000,
        };

        for _ in 0..3 {
            assert!(budget.affords(100));
        }
        assert!(!budget.affords(100));
    }

    #[test]
    fn no_type_expands_after_the_total_budget_runs_out() {
        let mut budget = Budget { wide: 1, total: 2 };

        assert!(budget.affords(NARROW_MEMBERS));
        assert!(budget.affords(NARROW_MEMBERS));
        assert!(!budget.affords(NARROW_MEMBERS));
    }
}
