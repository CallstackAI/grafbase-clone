use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_graphql::{Context, EmptySubscription, Object, Schema};

#[derive(Default)]
pub struct StateMutationSchema {
    state: Arc<AtomicUsize>,
}

impl StateMutationSchema {
    fn schema(&self) -> Schema<Query, Mutation, EmptySubscription> {
        Schema::build(Query, Mutation, EmptySubscription)
            .enable_federation()
            .data(Arc::clone(&self.state))
            .finish()
    }
}

#[async_trait::async_trait]
impl super::Schema for StateMutationSchema {
    async fn execute(
        &self,
        _headers: Vec<(String, String)>,
        request: async_graphql::Request,
    ) -> async_graphql::Response {
        self.schema().execute(request).await
    }

    fn sdl(&self) -> String {
        self.schema()
            .sdl_with_options(async_graphql::SDLExportOptions::new().federation())
    }
}

struct Query;

#[Object]
impl Query {
    async fn value(&self, ctx: &Context<'_>) -> usize {
        ctx.data_unchecked::<Arc<AtomicUsize>>().load(Ordering::Relaxed)
    }
}

struct Mutation;

#[Object]
impl Mutation {
    async fn multiply(&self, ctx: &Context<'_>, by: usize) -> usize {
        let state = ctx.data_unchecked::<Arc<AtomicUsize>>();
        state
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| Some(val * by))
            .unwrap();
        state.load(Ordering::Relaxed)
    }

    async fn set(&self, ctx: &Context<'_>, val: usize) -> usize {
        let state = ctx.data_unchecked::<Arc<AtomicUsize>>();
        state.store(val, Ordering::Relaxed);
        state.load(Ordering::Relaxed)
    }

    async fn fail(&self) -> async_graphql::FieldResult<usize> {
        Err("This mutation always fails".into())
    }

    async fn faillible(&self) -> async_graphql::FieldResult<Option<usize>> {
        Err("This mutation always fails".into())
    }
}