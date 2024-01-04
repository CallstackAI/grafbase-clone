use super::SchemaWalker;
use crate::{ScalarId, StringId};

pub type ScalarWalker<'a> = SchemaWalker<'a, ScalarId>;

impl<'a> ScalarWalker<'a> {
    pub fn name(&self) -> &'a str {
        self.names.scalar(self.schema, self.item)
    }

    pub fn specified_by_url(&self) -> Option<&'a str> {
        self.as_ref().specified_by_url.map(|id| self.schema[id].as_str())
    }

    pub fn specified_by_url_string_id(&self) -> Option<StringId> {
        self.as_ref().specified_by_url
    }

    pub fn description(&self) -> Option<&'a str> {
        self.as_ref().description.map(|id| self.schema[id].as_str())
    }
}

impl<'a> std::fmt::Debug for ScalarWalker<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scalar")
            .field("id", &usize::from(self.item))
            .field("name", &self.name())
            .field("description", &self.description())
            .field("specified_by_url", &self.specified_by_url())
            .finish()
    }
}
