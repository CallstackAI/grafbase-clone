use engine::Positioned;
use engine_parser::types::SchemaDefinition;

use super::{
    directive::Directive,
    visitor::{Visitor, VisitorContext},
};
use crate::directive_de::parse_directive;

const POSTGRES_DIRECTIVE_NAME: &str = "postgres";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresDirective {
    name: String,
    url: String,
    #[serde(default = "default_to_true")]
    namespace: bool,
}

fn default_to_true() -> bool {
    true
}

impl PostgresDirective {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn connection_string(&self) -> &str {
        &self.url
    }

    pub fn namespace(&self) -> bool {
        self.namespace
    }
}

impl Directive for PostgresDirective {
    fn definition() -> String {
        r#"
        directive @postgres(
          """
          A unique name for the given directive.
          """
          name: String!

          """
          The full connection string to the database.
          """
          url: String!
          
          """
          If true, namespaces queries and mutations with the
          connector name. Defaults to true.
          """
          namespace: Boolean
        ) on SCHEMA
        "#
        .to_string()
    }
}

pub struct PostgresVisitor;

impl<'a> Visitor<'a> for PostgresVisitor {
    fn enter_schema(&mut self, ctx: &mut VisitorContext<'a>, doc: &'a Positioned<SchemaDefinition>) {
        let directives = doc
            .node
            .directives
            .iter()
            .filter(|d| d.node.name.node == POSTGRES_DIRECTIVE_NAME);

        for directive in directives {
            match parse_directive::<PostgresDirective>(&directive.node, ctx.variables) {
                Ok(parsed_directive) => ctx.postgres_directives.push((parsed_directive, directive.pos)),
                Err(err) => ctx.report_error(vec![directive.pos], err.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futures::executor::block_on;

    use crate::{connector_parsers::MockConnectorParsers, rules::visitor::RuleError};

    macro_rules! assert_validation_error {
        ($schema:literal, $expected_message:literal) => {
            assert_matches!(
                crate::parse_registry($schema)
                    .err()
                    .and_then(crate::Error::validation_errors)
                    // We don't care whether there are more errors or not.
                    // It only matters that we find the expected error.
                    .and_then(|errors| errors.into_iter().next()),
                Some(RuleError { message, .. }) => {
                    assert_eq!(message, $expected_message);
                }
            );
        };
    }

    #[test]
    fn parsing_postgres_directive() {
        let variables = HashMap::from([(
            "PG_CONNECTION_STRING".to_string(),
            "postgres://postgres:grafbase@localhost:5432/postgres".to_string(),
        )]);

        let schema = r#"
            extend schema
              @postgres(
                name: "possu",
                namespace: true,
                url: "{{ env.PG_CONNECTION_STRING }}",
              )
            "#;

        let connector_parsers = MockConnectorParsers::default();

        block_on(crate::parse(schema, &variables, false, &connector_parsers)).unwrap();

        insta::assert_debug_snapshot!(connector_parsers.postgres_directives.lock().unwrap(), @r###"
        [
            PostgresDirective {
                name: "possu",
                url: "postgres://postgres:grafbase@localhost:5432/postgres",
                namespace: true,
            },
        ]
        "###);
    }

    #[test]
    fn test_parsing_directive_with_duplicate_name_with_graphql() {
        assert_validation_error!(
            r#"
            extend schema
              @postgres(
                name: "Test",
                namespace: true,
                url: "postgres://postgres:grafbase@localhost:5432/postgres",
              )

            extend schema
              @graphql(
                name: "Test",
                namespace: true,
                url: "https://countries.trevorblades.com",
              )
            "#,
            "Name \"Test\" is not unique. A connector must have a unique name."
        );
    }

    #[test]
    fn test_parsing_directive_with_duplicate_name_with_mongo() {
        assert_validation_error!(
            r#"
            extend schema
              @postgres(
                name: "Test",
                namespace: true,
                url: "postgres://postgres:grafbase@localhost:5432/postgres",
              )

            extend schema
              @mongodb(
                 name: "Test",
                 apiKey: "TEST"
                 url: "TEST"
                 dataSource: "TEST"
                 database: "TEST"
                 namespace: true,
              )
            "#,
            "Name \"Test\" is not unique. A connector must have a unique name."
        );
    }

    #[test]
    fn test_parsing_directive_with_duplicate_name_with_openapi() {
        assert_validation_error!(
            r#"
            extend schema
              @postgres(
                name: "Test",
                namespace: true,
                url: "postgres://postgres:grafbase@localhost:5432/postgres",
              )

            extend schema
              @openapi(
                name: "Test",
                namespace: true,
                schema: "https://raw.githubusercontent.com/stripe/openapi/master/openapi/spec3.json",
                url: "https://api.stripe.com",
              )
            "#,
            "Name \"Test\" is not unique. A connector must have a unique name."
        );
    }
}