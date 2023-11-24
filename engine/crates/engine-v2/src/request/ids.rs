use super::{BoundField, BoundFieldDefinition, BoundFragmentDefinition, BoundSelectionSet, Operation};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, derive_more::Display)]
pub struct BoundFieldDefinitionId(u32);

impl From<usize> for BoundFieldDefinitionId {
    fn from(value: usize) -> Self {
        BoundFieldDefinitionId(value.try_into().expect("Too many fields."))
    }
}

impl From<BoundFieldDefinitionId> for usize {
    fn from(value: BoundFieldDefinitionId) -> Self {
        value.0 as usize
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, derive_more::Display)]
pub struct BoundFragmentDefinitionId(u16);

impl From<usize> for BoundFragmentDefinitionId {
    fn from(value: usize) -> Self {
        BoundFragmentDefinitionId(value.try_into().expect("Too many fragments."))
    }
}

impl From<BoundFragmentDefinitionId> for usize {
    fn from(value: BoundFragmentDefinitionId) -> Self {
        value.0 as usize
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, derive_more::Display)]
pub struct BoundSelectionSetId(u16);

impl From<usize> for BoundSelectionSetId {
    fn from(value: usize) -> Self {
        BoundSelectionSetId(value.try_into().expect("Too many selection sets."))
    }
}

impl From<BoundSelectionSetId> for usize {
    fn from(value: BoundSelectionSetId) -> Self {
        value.0 as usize
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, derive_more::Display)]
pub struct BoundFieldId(u32);

impl From<usize> for BoundFieldId {
    fn from(value: usize) -> Self {
        BoundFieldId(value.try_into().expect("Too many spreaded fields."))
    }
}

impl From<BoundFieldId> for usize {
    fn from(value: BoundFieldId) -> Self {
        value.0 as usize
    }
}

impl std::ops::Index<BoundFieldId> for Operation {
    type Output = BoundField;

    fn index(&self, index: BoundFieldId) -> &Self::Output {
        &self.fields[index.0 as usize]
    }
}

impl std::ops::Index<BoundSelectionSetId> for Operation {
    type Output = BoundSelectionSet;

    fn index(&self, index: BoundSelectionSetId) -> &Self::Output {
        &self.selection_sets[index.0 as usize]
    }
}

impl std::ops::Index<BoundFieldDefinitionId> for Operation {
    type Output = BoundFieldDefinition;

    fn index(&self, index: BoundFieldDefinitionId) -> &Self::Output {
        &self.field_definitions[index.0 as usize]
    }
}

impl std::ops::Index<BoundFragmentDefinitionId> for Operation {
    type Output = BoundFragmentDefinition;

    fn index(&self, index: BoundFragmentDefinitionId) -> &Self::Output {
        &self.fragment_definitions[index.0 as usize]
    }
}