use async_graphql::Context;
use engine::registry::CacheTag;
use send_wrapper::SendWrapper;

use crate::admin::{
    error::AdminError, graphql::cache::mutation::input::CachePurgeTypesInput, WrappedCache, WrappedContext,
};

mod input {
    #[derive(Debug, async_graphql::InputObject)]
    pub struct PurgeEntityFieldsInput {
        pub name: String,
        pub value: String,
    }

    #[derive(Debug, async_graphql::InputObject)]
    pub struct PurgeEntityInput {
        #[graphql(name = "type")]
        pub cache_type: String,
        pub fields: Vec<PurgeEntityFieldsInput>,
    }

    #[derive(Debug, async_graphql::InputObject)]
    pub struct PurgeListInput {
        #[graphql(name = "type")]
        pub cache_type: String,
    }

    #[derive(Debug, async_graphql::InputObject)]
    pub struct PurgeTypeInput {
        #[graphql(name = "type")]
        pub cache_type: String,
    }

    #[derive(Debug, async_graphql::OneofObject)]
    pub enum CachePurgeTypesInput {
        Type(PurgeTypeInput),
        List(PurgeListInput),
        Entity(PurgeEntityInput),
    }
}

mod output {
    #[derive(Debug, async_graphql::SimpleObject)]
    pub struct CachePurgeTypes {
        pub tags: Vec<String>,
    }

    #[derive(Debug, async_graphql::SimpleObject)]
    pub struct CachePurgeDomain {
        pub hostname: String,
    }
}

#[derive(Debug, Default)]
pub struct CachePurgeMutation;

#[async_graphql::Object]
impl CachePurgeMutation {
    pub async fn cache_purge_types(
        &self,
        ctx: &Context<'_>,
        input: CachePurgeTypesInput,
    ) -> Result<output::CachePurgeTypes, AdminError> {
        let global_cache_provider = ctx
            .data::<WrappedCache>()
            .map_err(|_e| AdminError::CachePurgeError("Missing cache provider".to_string()))?;

        let request_context = ctx
            .data::<WrappedContext>()
            .map_err(|_e| AdminError::CachePurgeError("Missing request context".to_string()))?;

        let cache_tags: Vec<String> = match input {
            CachePurgeTypesInput::Type(type_purge) => vec![CacheTag::Type {
                type_name: type_purge.cache_type,
            }
            .to_string()],
            CachePurgeTypesInput::List(list_purge) => vec![CacheTag::List {
                type_name: list_purge.cache_type,
            }
            .to_string()],
            CachePurgeTypesInput::Entity(entity_purge) => entity_purge
                .fields
                .into_iter()
                .map(|field| {
                    CacheTag::Field {
                        type_name: entity_purge.cache_type.clone(),
                        field_name: field.name,
                        value: field.value,
                    }
                    .to_string()
                })
                .collect(),
        };

        log::info!(request_context.ray_id(), "Purging cache tags: {:?}", cache_tags);

        let send_purge_future = SendWrapper::new(global_cache_provider.purge_by_tags(cache_tags.clone()));

        send_purge_future
            .await
            .map_err(|e| AdminError::CachePurgeError(e.to_string()))?;

        log::info!(request_context.ray_id(), "Successfully purged tags");

        Ok(output::CachePurgeTypes { tags: cache_tags })
    }

    pub async fn cache_purge_all(&self, ctx: &Context<'_>) -> Result<output::CachePurgeDomain, AdminError> {
        let global_cache_provider = ctx
            .data::<WrappedCache>()
            .map_err(|_e| AdminError::CachePurgeError("Missing cache provider".to_string()))?;

        let request_context = ctx
            .data::<WrappedContext>()
            .map_err(|_e| AdminError::CachePurgeError("Missing request context".to_string()))?;

        log::info!(
            request_context.ray_id(),
            "Purging cache for host: {:?}",
            request_context.host_name(),
        );

        let send_purge_future =
            SendWrapper::new(global_cache_provider.purge_by_hostname(request_context.host_name().to_string()));

        send_purge_future
            .await
            .map_err(|e| AdminError::CachePurgeError(e.to_string()))?;

        log::info!(request_context.ray_id(), "Successfully purged host");

        Ok(output::CachePurgeDomain {
            hostname: request_context.host_name().to_string(),
        })
    }
}