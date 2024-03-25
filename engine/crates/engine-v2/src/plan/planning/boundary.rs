use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
};

use itertools::Itertools;
use schema::{FieldDefinitionId, RequiredField, RequiredFieldSet, ResolverId, ResolverWalker};

use super::{logic::PlanningLogic, planner::Planner, PlanningError, PlanningResult};
use crate::{
    operation::{Field, FieldId, QueryPath, Selection, SelectionSet, SelectionSetId, SelectionSetType},
    plan::{flatten_selection_sets, EntityType, FlatField, FlatSelectionSet, ParentToChildEdge, PlanId},
    response::{ReadField, ReadSelectionSet, UnpackedResponseEdge},
};

/// The Planner traverses the selection sets to plan all the fields, but it doesn't define the
/// plans directly. That's the job of the BoundaryPlanner which will attribute a plan for each
/// field for a given selection set and satisfy any requirements.
pub(super) struct BoundaryPlanner<'schema, 'a> {
    planner: &'a mut Planner<'schema>,
    query_path: &'a QueryPath,
    maybe_parent: Option<&'a PlanningLogic<'schema>>,
    children: Vec<PlanId>,
}

impl<'schema, 'a> BoundaryPlanner<'schema, 'a> {
    pub(super) fn plan(
        planner: &'a mut Planner<'schema>,
        query_path: &'a QueryPath,
        maybe_parent: Option<BoundaryParent<'schema, 'a>>,
        unplanned: FlatSelectionSet,
    ) -> PlanningResult<Vec<PlanId>> {
        if let Some(BoundaryParent { logic, providable }) = maybe_parent {
            let boundary_planner = Self {
                planner,
                query_path,
                maybe_parent: Some(logic),
                children: Vec::new(),
            };
            let mut boundary_fields = boundary_planner.create_boundary_fields(providable)?;
            boundary_planner.plan_selection_set(&mut boundary_fields, unplanned)
        } else {
            Self {
                planner,
                query_path,
                maybe_parent: None,
                children: Vec::new(),
            }
            .plan_selection_set(&mut BoundaryFields::default(), unplanned)
        }
    }

    fn create_boundary_fields(&self, providable: FlatSelectionSet) -> PlanningResult<BoundaryFields> {
        let grouped = self
            .walker()
            .group_by_definition_id_sorted_by_query_position(providable.into_iter().map(|field| field.id));

        let mut fields = BoundaryFields::default();
        for (definition_id, field_ids) in grouped {
            // It doesn't matter which one we take, all fields with the same field id
            // will necessarily be resolved by the same plan.
            let field_id = field_ids[0];

            // At this stage we're generating boundary fields for an existing selection set which
            // was already planned. By construction, as soon as we create a new plan with
            // push_plan() it plans all of the nested selection sets.
            // And for extra fields we add during planning, those are attributed immediately.
            let plan_id = self.get_field_plan(field_id).expect("field should be planned");
            fields.insert(definition_id, BoundaryField::new(plan_id, field_ids));
        }
        Ok(fields)
    }
}

pub(super) struct BoundaryParent<'schema, 'a> {
    pub logic: &'a PlanningLogic<'schema>,
    pub providable: FlatSelectionSet,
}

impl<'schema, 'a> std::ops::Deref for BoundaryPlanner<'schema, 'a> {
    type Target = Planner<'schema>;
    fn deref(&self) -> &Self::Target {
        self.planner
    }
}

impl<'schema, 'a> std::ops::DerefMut for BoundaryPlanner<'schema, 'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.planner
    }
}

/// During the planning of the boundary we need to keep track of fields by their FieldId
/// to satisfy requirements. The goal is not only to know what's present but also to have the
/// correct ResponseEdge for those when reading data from the response later.
type BoundaryFields = HashMap<FieldDefinitionId, BoundaryField>;

#[derive(Debug)]
struct BoundaryField {
    plan_id: PlanId,
    field_ids: Vec<FieldId>,
    lazy_subselection: Option<BoundaryFields>,
}

impl BoundaryField {
    pub(super) fn new(plan_id: PlanId, field_ids: Vec<FieldId>) -> Self {
        Self {
            plan_id,
            field_ids,
            lazy_subselection: None,
        }
    }
}

/// Potential child plan, but might not be the best one.
struct ChildPlanCandidate<'schema> {
    resolver_id: ResolverId,
    /// Entity type (object/interface id) of the fields
    entity_type: EntityType,
    /// Providable fields by the resolvers with their requirements
    providable_fields: Vec<(FieldId, &'schema RequiredFieldSet)>,
}

/// Field that the parent plan could not providabe.
struct UnplannedField {
    entity_type: EntityType,
    flat_field: FlatField,
    definition_id: FieldDefinitionId,
}

impl std::ops::Deref for UnplannedField {
    type Target = FlatField;
    fn deref(&self) -> &Self::Target {
        &self.flat_field
    }
}

impl From<UnplannedField> for FlatField {
    fn from(unplanned: UnplannedField) -> Self {
        unplanned.flat_field
    }
}

impl<'schema, 'a> BoundaryPlanner<'schema, 'a> {
    /// Iteratively plan fields.
    /// 1. Generate all potential plan candidates satisfying their requirements if possible.
    /// 2. Select the best candidate, generate its input and remove its output fields from the
    ///    unplanned ones.
    /// 3. Continue until there are no more unplanned fields.
    fn plan_selection_set(
        mut self,
        boundary_fields: &mut BoundaryFields,
        mut unplanned_selection_set: FlatSelectionSet,
    ) -> PlanningResult<Vec<PlanId>> {
        // Fields that couldn't be provided by the parent and that have yet to be planned by one
        // child plan.
        let mut id_to_unplanned_fields: HashMap<FieldId, UnplannedField> =
            self.build_unplanned_fields(std::mem::take(&mut unplanned_selection_set.fields));

        // Actual planning, we plan one child plan at a time.
        let mut candidates: HashMap<ResolverId, ChildPlanCandidate<'schema>> = HashMap::default();
        while !id_to_unplanned_fields.is_empty() {
            candidates.clear();
            self.generate_all_candidates(id_to_unplanned_fields.values(), boundary_fields, &mut candidates)?;

            let Some(candidate) = select_best_child_plan(&mut candidates) else {
                let walker = self.walker();
                tracing::debug!(
                    "Could not plan fields:\n=== PARENT ===\n{:#?}\n=== CURRENT ===\n{}\n=== MISSING ===\n{}",
                    self.maybe_parent.map(|parent| parent.resolver()),
                    boundary_fields
                        .keys()
                        .map(|id| self.schema.walk(*id))
                        .format_with("\n", |field, f| f(&format_args!("{field:#?}"))),
                    id_to_unplanned_fields
                        .keys()
                        .map(|id| walker.walk(*id).definition().unwrap())
                        .format_with("\n", |field, f| f(&format_args!("{field:#?}")))
                );
                return Err(PlanningError::CouldNotPlanAnyField {
                    missing: id_to_unplanned_fields
                        .into_keys()
                        .map(|id| walker.walk(id).response_key_str().to_string())
                        .collect(),
                    query_path: walker.walk(self.query_path).iter().map(|s| s.to_string()).collect(),
                });
            };

            let mut requires = Cow::Borrowed(self.schema.walk(candidate.resolver_id).requires());
            let mut output = vec![];
            for (id, field_requires) in std::mem::take(&mut candidate.providable_fields) {
                let flat_field = FlatField::from(id_to_unplanned_fields.remove(&id).unwrap());
                if !field_requires.is_empty() {
                    requires = Cow::Owned(requires.union(field_requires));
                }
                output.push(flat_field);
            }
            let output = unplanned_selection_set.clone_with_fields(output);
            self.push_child(candidate, requires, output, boundary_fields)?;
        }

        Ok(self.children)
    }

    fn push_child(
        &mut self,
        candidate: &mut ChildPlanCandidate<'schema>,
        requires: Cow<'_, RequiredFieldSet>,
        providable: FlatSelectionSet,
        boundary_fields: &mut BoundaryFields,
    ) -> PlanningResult<()> {
        let path = self.query_path.clone();
        let plan_id = self.push_plan(path, candidate.resolver_id, candidate.entity_type, &providable)?;
        if !requires.is_empty() {
            let resolver = self.schema.walker().walk(candidate.resolver_id).with_own_names();
            let input_selection_set = self.create_input_selection_set(plan_id, &resolver, &requires, boundary_fields);
            self.insert_plan_input_selection_set(plan_id, input_selection_set);
        };
        let field_ids_grouped_by_definition_id = self
            .walker()
            .group_by_definition_id_sorted_by_query_position(providable.fields.iter().map(|field| field.id));
        for (definition_id, field_ids) in field_ids_grouped_by_definition_id {
            boundary_fields
                .entry(definition_id)
                .or_insert_with(|| BoundaryField::new(plan_id, field_ids));
        }

        self.children.push(plan_id);
        Ok(())
    }

    /// Create the input selection set of a Plan given its resolver and requirements.
    /// We iterate over the requirements and find the matching fields inside the boundary fields,
    /// which contains all providable & extra fields. During the iteration we track all the dependency
    /// plans.
    fn create_input_selection_set(
        &mut self,
        plan_id: PlanId,
        resolver: &ResolverWalker<'_>,
        requires: &RequiredFieldSet,
        boundary_fields: &BoundaryFields,
    ) -> ReadSelectionSet {
        if requires.is_empty() {
            return ReadSelectionSet::default();
        }
        requires
            .iter()
            .map(|field| {
                let boundary_field = boundary_fields
                    .get(&field.definition_id)
                    .expect("field should be present, we could plan it");
                let field_id = boundary_field.field_ids[0];
                // We add a bunch of fields during the planning to the operation when trying to
                // satisfy requirements. But only those marked as read will be retrieved.
                self.operation[field_id].mark_as_read();
                self.insert_plan_dependency(ParentToChildEdge {
                    parent: boundary_field.plan_id,
                    child: plan_id,
                });
                ReadField {
                    edge: self.operation[field_id].response_edge(),
                    name: resolver.walk(field.definition_id).name().to_string(),
                    subselection: if field.subselection.is_empty() {
                        ReadSelectionSet::default()
                    } else {
                        let subselection = boundary_field
                            .lazy_subselection
                            .as_ref()
                            .expect("subselection should be present, we could plan the subselection");
                        self.create_input_selection_set(plan_id, resolver, &field.subselection, subselection)
                    },
                }
            })
            .collect()
    }

    fn build_unplanned_fields(&self, fields: Vec<FlatField>) -> HashMap<FieldId, UnplannedField> {
        let mut id_to_unplanned_fields = HashMap::default();
        for flat_field in fields {
            let entity_type = match self.operation[flat_field.parent_selection_set_id()].ty {
                SelectionSetType::Object(id) => EntityType::Object(id),
                SelectionSetType::Interface(id) => EntityType::Interface(id),
                SelectionSetType::Union(_) => unreachable!("Unions have no fields."),
            };
            let definition_id = self.operation[flat_field.id]
                .definition_id()
                .expect("Meta fields are always providable, it can't be missing.");
            id_to_unplanned_fields.insert(
                flat_field.id,
                UnplannedField {
                    entity_type,
                    flat_field,
                    definition_id,
                },
            );
        }
        id_to_unplanned_fields
    }

    fn generate_all_candidates<'field>(
        &mut self,
        unplanned_fields: impl IntoIterator<Item = &'field UnplannedField>,
        boundary_fields: &mut BoundaryFields,
        candidates: &mut HashMap<ResolverId, ChildPlanCandidate<'schema>>,
    ) -> PlanningResult<()>
    where
        'schema: 'field,
    {
        for field in unplanned_fields {
            let definition = self.schema.walk(field.definition_id);
            for resolver in definition.resolvers() {
                let field_requires = definition.requires(resolver.subgraph_id());
                match candidates.entry(resolver.id()) {
                    Entry::Occupied(mut entry) => {
                        let candidate = entry.get_mut();
                        if self.could_plan_requirements(boundary_fields, field.id, field_requires)? {
                            candidate.providable_fields.push((field.id, field_requires));
                        }
                    }
                    Entry::Vacant(entry) => {
                        if self.could_plan_requirements(boundary_fields, field.id, resolver.requires())?
                            && self.could_plan_requirements(boundary_fields, field.id, field_requires)?
                        {
                            entry.insert(ChildPlanCandidate {
                                entity_type: field.entity_type,
                                resolver_id: resolver.id(),
                                providable_fields: vec![(field.id, field_requires)],
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Allows us to know whether a field requirements can be provided at all to order the next child
    /// candidates.
    fn could_plan_requirements(
        &mut self,
        boundary_fields: &mut BoundaryFields,
        origin_field_id: FieldId,
        requires: &RequiredFieldSet,
    ) -> PlanningResult<bool> {
        if requires.is_empty() {
            return Ok(true);
        }
        let parent_field_plan_id = self
            .maybe_parent
            .expect("Cannot have requirements without a parent plan")
            .plan_id();
        self.could_plan_requirements_on_previous_plans(parent_field_plan_id, boundary_fields, origin_field_id, requires)
    }

    fn could_plan_requirements_on_previous_plans(
        &mut self,
        parent_field_plan_id: PlanId,
        boundary_fields: &mut BoundaryFields,
        field_id: FieldId,
        requires: &RequiredFieldSet,
    ) -> PlanningResult<bool> {
        if requires.is_empty() {
            return Ok(true);
        }
        let parent_selection_set_id = self.operation.parent_selection_set_id(field_id);
        'requires: for required_field in requires.iter() {
            // -- Existing fields --
            if let Some(boundary_field) = boundary_fields.get_mut(&required_field.definition_id) {
                if required_field.subselection.is_empty() {
                    continue;
                }
                if boundary_field.lazy_subselection.is_none() {
                    let subselection_set_ids = boundary_field
                        .field_ids
                        .iter()
                        .filter_map(|id| self.operation[*id].selection_set_id())
                        .collect();
                    let flat_selection_set = flatten_selection_sets(self.schema, &self.operation, subselection_set_ids);
                    let fields = self.create_boundary_fields(flat_selection_set)?;
                    boundary_field.lazy_subselection = Some(fields)
                }
                if self.could_plan_requirements_on_previous_plans(
                    boundary_field.plan_id,
                    boundary_field.lazy_subselection.as_mut().unwrap(),
                    boundary_field.field_ids[0],
                    &required_field.subselection,
                )? {
                    continue;
                } else {
                    return Ok(false);
                }
            }

            // -- Plannable by the parent --
            let field = self.schema.walker().walk(required_field.definition_id);
            let parent_logic = self
                .maybe_parent
                .expect("Cannot have requirements without a parent plan");
            // no need to check for requires here, they're only relevant when it's a
            // plan root field and this is a nested field. So we expect the data source
            // to be able to provide anything it needed for a nested object it provides.
            if parent_logic.plan_id() == parent_field_plan_id && parent_logic.is_providable(field.id()) {
                if let Some(boundary_field) =
                    self.try_planning_boundary_field(parent_logic, parent_selection_set_id, required_field)
                {
                    boundary_fields.insert(field.id(), boundary_field);
                    continue;
                }
            }

            // -- Plannable by existing children --
            for i in 0..self.children.len() {
                let plan_id = self.children[i];
                // ensures we don't have cycles between plans ensuring they can only depend on
                // plan_ids lower than theirs. Could be better.
                if plan_id <= parent_field_plan_id {
                    continue;
                }
                let resolver_id = self.get_planned_resolver(plan_id).resolver_id;
                let logic = &PlanningLogic::new(plan_id, self.schema.walk(resolver_id));
                if let Some(boundary_field) =
                    self.try_planning_boundary_field(logic, parent_selection_set_id, required_field)
                {
                    boundary_fields.insert(field.id(), boundary_field);
                    continue 'requires;
                }
            }

            // -- Not plannable --
            return Ok(false);
        }

        Ok(true)
    }

    fn try_planning_boundary_field(
        &mut self,
        logic: &PlanningLogic<'schema>,
        parent_selection_set_id: SelectionSetId,
        item: &RequiredField,
    ) -> Option<BoundaryField> {
        self.try_planning_extra_fields_with_subselection(logic, Some(parent_selection_set_id), item)
            .map(|field_id| BoundaryField {
                plan_id: logic.plan_id(),
                field_ids: vec![field_id],
                lazy_subselection: None,
            })
    }

    fn try_planning_extra_fields_with_subselection(
        &mut self,
        logic: &PlanningLogic<'schema>,
        parent_selection_set_id: Option<SelectionSetId>,
        required_field: &RequiredField,
    ) -> Option<FieldId> {
        // We don't
        if !logic.is_providable(required_field.definition_id) {
            return None;
        }
        let field = logic.resolver().walk(required_field.definition_id);
        let selection_set_id = if let Some(ty) = SelectionSetType::maybe_from(field.ty().inner().id()) {
            let logic = logic.child(field.id());
            for _item in &required_field.subselection {
                // Not need to check field requirements, it's nested a field, so the resolver is
                // expected to provide anything it needs.
                if !logic.is_providable(field.id()) {
                    return None;
                }
            }
            let selection_set = SelectionSet {
                ty,
                items: required_field
                    .subselection
                    .iter()
                    .map(|item| {
                        self.try_planning_extra_fields_with_subselection(&logic, None, item)
                            .map(Selection::Field)
                    })
                    .collect::<Option<Vec<_>>>()?,
            };
            Some(self.push_extra_selection_set(logic.plan_id(), selection_set))
        } else {
            None
        };
        tracing::debug!(
            "Adding extra field '{}' provided by {}",
            self.schema.walker().walk(required_field.definition_id).name(),
            logic.plan_id()
        );
        let key = self.generate_unique_response_key_for(required_field.definition_id);
        let field = Field::Extra {
            edge: UnpackedResponseEdge::ExtraFieldResponseKey(key.into()).pack(),
            field_definition_id: required_field.definition_id,
            selection_set_id,
            is_read: true,
        };
        Some(self.push_extra_field(logic.plan_id(), parent_selection_set_id, field))
    }
}

fn select_best_child_plan<'c, 'op>(
    candidates: &'c mut HashMap<ResolverId, ChildPlanCandidate<'op>>,
) -> Option<&'c mut ChildPlanCandidate<'op>> {
    // We could be smarter, but we need to be sure there is no intersection between
    // candidates (which impacts ordering among other things) and some fields may now be
    // available (requires can now be provided) after planning this candidate. So the easy
    // solution is to regenerate candidates after each plan.
    candidates
        .values_mut()
        .filter(|candidate| !candidate.providable_fields.is_empty())
        .max_by_key(|candidate| candidate.providable_fields.len())
}
