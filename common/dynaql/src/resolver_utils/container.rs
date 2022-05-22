use futures_util::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::extensions::ResolveInfo;
use crate::parser::types::Selection;
use crate::registry::MetaType;
use crate::{
    Context, ContextBase, ContextSelectionSet, Error, Name, OutputType, ServerError, ServerResult,
    Value,
};

/// Represents a GraphQL container object.
///
/// This helper trait allows the type to call `resolve_container` on itself in its
/// `OutputType::resolve` implementation.
#[async_trait::async_trait]
pub trait ContainerType: OutputType {
    /// This function returns true of type `EmptyMutation` only.
    #[doc(hidden)]
    fn is_empty() -> bool {
        false
    }

    /// Resolves a field value and outputs it as a json value `async_graphql::Value`.
    ///
    /// If the field was not found returns None.
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>>;

    /// Collect all the fields of the container that are queried in the selection set.
    ///
    /// Objects do not have to override this, but interfaces and unions must call it on their
    /// internal type.
    fn collect_all_fields_native<'a>(
        &'a self,
        ctx: &ContextSelectionSet<'a>,
        fields: &mut Fields<'a>,
    ) -> ServerResult<()>
    where
        Self: Send + Sync,
    {
        fields.add_set_native(ctx, self)
    }

    /// Find the GraphQL entity with the given name from the parameter.
    ///
    /// Objects should override this in case they are the query root.
    async fn find_entity(&self, _: &Context<'_>, _params: &Value) -> ServerResult<Option<Value>> {
        Ok(None)
    }
}

/// Collect all the fields of the container that are queried in the selection set.
///
/// Objects do not have to override this, but interfaces and unions must call it on their
/// internal type.
fn collect_all_fields_meta<'a>(
    ty: &'a MetaType,
    ctx: &ContextSelectionSet<'a>,
    fields: &mut Fields<'a>,
) -> ServerResult<()> {
    fields.add_set(ctx, ty)
}

#[async_trait::async_trait]
impl<T: ContainerType + ?Sized> ContainerType for &T {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(*self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(*self, ctx, params).await
    }
}

#[async_trait::async_trait]
impl<T: ContainerType + ?Sized> ContainerType for Arc<T> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(self, ctx, params).await
    }
}

#[async_trait::async_trait]
impl<T: ContainerType + ?Sized> ContainerType for Box<T> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(self, ctx, params).await
    }
}

#[async_trait::async_trait]
impl<T: ContainerType, E: Into<Error> + Send + Sync + Clone> ContainerType for Result<T, E> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        match self {
            Ok(value) => T::resolve_field(value, ctx).await,
            Err(err) => Err(ctx.set_error_path(err.clone().into().into_server_error(ctx.item.pos))),
        }
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        match self {
            Ok(value) => T::find_entity(value, ctx, params).await,
            Err(err) => Err(ctx.set_error_path(err.clone().into().into_server_error(ctx.item.pos))),
        }
    }
}

/// Resolve an container by executing each of the fields concurrently.
pub async fn resolve_container<'a>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a MetaType,
) -> ServerResult<Value> {
    resolve_container_inner(ctx, true, root).await
}

/// Resolve an container by executing each of the fields serially.
pub async fn resolve_container_serial<'a>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a MetaType,
) -> ServerResult<Value> {
    resolve_container_inner(ctx, false, root).await
}

/// Resolve an container by executing each of the fields concurrently.
pub async fn resolve_container_native<'a, T: ContainerType + ?Sized>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a T,
) -> ServerResult<Value> {
    resolve_container_inner_native(ctx, root, true).await
}

/// Resolve an container by executing each of the fields serially.
pub async fn resolve_container_serial_native<'a, T: ContainerType + ?Sized>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a T,
) -> ServerResult<Value> {
    resolve_container_inner_native(ctx, root, false).await
}

fn insert_value(target: &mut IndexMap<Name, Value>, name: Name, value: Value) {
    if let Some(prev_value) = target.get_mut(&name) {
        if let Value::Object(target_map) = prev_value {
            if let Value::Object(obj) = value {
                for (key, value) in obj.into_iter() {
                    insert_value(target_map, key, value);
                }
            }
        } else if let Value::List(target_list) = prev_value {
            if let Value::List(list) = value {
                for (idx, value) in list.into_iter().enumerate() {
                    if let Some(Value::Object(target_map)) = target_list.get_mut(idx) {
                        if let Value::Object(obj) = value {
                            for (key, value) in obj.into_iter() {
                                insert_value(target_map, key, value);
                            }
                        }
                    }
                }
            }
        }
    } else {
        target.insert(name, value);
    }
}

async fn resolve_container_inner<'a>(
    ctx: &ContextSelectionSet<'a>,
    parallel: bool,
    root: &'a MetaType,
) -> ServerResult<Value> {
    let mut fields = Fields(Vec::new());
    fields.add_set(ctx, root)?;

    let res = if parallel {
        futures_util::future::try_join_all(fields.0).await?
    } else {
        let mut results = Vec::with_capacity(fields.0.len());
        for field in fields.0 {
            results.push(field.await?);
        }
        results
    };

    let mut map = IndexMap::new();
    for (name, value) in res {
        insert_value(&mut map, name, value);
    }
    Ok(Value::Object(map))
}

async fn resolve_container_inner_native<'a, T: ContainerType + ?Sized>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a T,
    parallel: bool,
) -> ServerResult<Value> {
    let mut fields = Fields(Vec::new());
    fields.add_set_native(ctx, root)?;

    let res = if parallel {
        futures_util::future::try_join_all(fields.0).await?
    } else {
        let mut results = Vec::with_capacity(fields.0.len());
        for field in fields.0 {
            results.push(field.await?);
        }
        results
    };

    let mut map = IndexMap::new();
    for (name, value) in res {
        insert_value(&mut map, name, value);
    }
    Ok(Value::Object(map))
}

type BoxFieldFuture<'a> = Pin<Box<dyn Future<Output = ServerResult<(Name, Value)>> + 'a + Send>>;

/// A set of fields on an container that are being selected.
pub struct Fields<'a>(Vec<BoxFieldFuture<'a>>);

impl<'a> Fields<'a> {
    /// Add another set of fields to this set of fields using the given container.
    pub fn add_set(
        &mut self,
        ctx: &ContextSelectionSet<'a>,
        root: &'a MetaType,
    ) -> ServerResult<()> {
        let registry = ctx.registry();

        for selection in &ctx.item.node.items {
            match &selection.node {
                Selection::Field(field) => {
                    if field.node.name.node == "__typename" {
                        // Get the typename
                        let ctx_field = ctx.with_field(field, Some(root), Some(&ctx.item.node));
                        let field_name = ctx_field.item.node.response_key().node.clone();
                        let typename = registry.introspection_type_name(root).to_owned();

                        self.0.push(Box::pin(async move {
                            Ok((field_name, Value::String(typename)))
                        }));
                        continue;
                    }

                    let resolve_fut = Box::pin({
                        let ctx = ctx.clone();
                        async move {
                            let ctx_field = ctx.with_field(field, Some(root), Some(&ctx.item.node));
                            let registry = ctx_field.registry();
                            let field_name = ctx_field.item.node.response_key().node.clone();
                            let extensions = &ctx.query_env.extensions;

                            if extensions.is_empty() && field.node.directives.is_empty() {
                                Ok((
                                    field_name,
                                    registry
                                        .resolve_field(&ctx_field, root)
                                        .await?
                                        .unwrap_or_default(),
                                ))
                            } else {
                                let type_name = root.name();
                                let resolve_info = ResolveInfo {
                                    path_node: ctx_field.path_node.as_ref().unwrap(),
                                    parent_type: &type_name,
                                    return_type: match ctx_field
                                        .schema_env
                                        .registry
                                        .types
                                        .get(type_name)
                                        .and_then(|ty| {
                                            ty.field_by_name(field.node.name.node.as_str())
                                        })
                                        .map(|field| &field.ty)
                                    {
                                        Some(ty) => &ty,
                                        None => {
                                            return Err(ServerError::new(
                                                format!(
                                                    r#"Cannot query field "{}" on type "{}"."#,
                                                    field_name, type_name
                                                ),
                                                Some(ctx_field.item.pos),
                                            ));
                                        }
                                    },
                                    name: field.node.name.node.as_str(),
                                    alias: field
                                        .node
                                        .alias
                                        .as_ref()
                                        .map(|alias| alias.node.as_str()),
                                };

                                let resolve_fut = registry.resolve_field(&ctx_field, root);

                                if field.node.directives.is_empty() {
                                    futures_util::pin_mut!(resolve_fut);
                                    Ok((
                                        field_name,
                                        extensions
                                            .resolve(resolve_info, &mut resolve_fut)
                                            .await?
                                            .unwrap_or_default(),
                                    ))
                                } else {
                                    let mut resolve_fut = resolve_fut.boxed();

                                    for directive in &field.node.directives {
                                        if let Some(directive_factory) = ctx
                                            .schema_env
                                            .custom_directives
                                            .get(directive.node.name.node.as_str())
                                        {
                                            let ctx_directive = ContextBase {
                                                path_node: ctx_field.path_node,
                                                resolver_node: ctx_field.resolver_node.clone(),
                                                item: directive,
                                                schema_env: ctx_field.schema_env,
                                                query_env: ctx_field.query_env,
                                                resolvers_cache: ctx_field.resolvers_cache.clone(),
                                                resolvers_data: ctx_field.resolvers_data.clone(),
                                            };
                                            let directive_instance = directive_factory
                                                .create(&ctx_directive, &directive.node)?;
                                            resolve_fut = Box::pin({
                                                let ctx_field = ctx_field.clone();
                                                async move {
                                                    directive_instance
                                                        .resolve_field(&ctx_field, &mut resolve_fut)
                                                        .await
                                                }
                                            });
                                        }
                                    }

                                    Ok((
                                        field_name,
                                        extensions
                                            .resolve(resolve_info, &mut resolve_fut)
                                            .await?
                                            .unwrap_or_default(),
                                    ))
                                }
                            }
                        }
                    });

                    self.0.push(resolve_fut);
                }
                selection => {
                    let (type_condition, selection_set) = match selection {
                        Selection::Field(_) => unreachable!(),
                        Selection::FragmentSpread(spread) => {
                            let fragment =
                                ctx.query_env.fragments.get(&spread.node.fragment_name.node);
                            let fragment = match fragment {
                                Some(fragment) => fragment,
                                None => {
                                    return Err(ServerError::new(
                                        format!(
                                            r#"Unknown fragment "{}"."#,
                                            spread.node.fragment_name.node
                                        ),
                                        Some(spread.pos),
                                    ));
                                }
                            };
                            (
                                Some(&fragment.node.type_condition),
                                &fragment.node.selection_set,
                            )
                        }
                        Selection::InlineFragment(fragment) => (
                            fragment.node.type_condition.as_ref(),
                            &fragment.node.selection_set,
                        ),
                    };
                    let type_condition =
                        type_condition.map(|condition| condition.node.on.node.as_str());

                    let introspection_type_name = registry.introspection_type_name(root);

                    let applies_concrete_object = type_condition.map_or(false, |condition| {
                        introspection_type_name == condition
                            || ctx
                                .schema_env
                                .registry
                                .implements
                                .get(&*introspection_type_name)
                                .map_or(false, |interfaces| interfaces.contains(condition))
                    });
                    if applies_concrete_object {
                        collect_all_fields_meta(
                            root,
                            &ctx.with_selection_set(selection_set),
                            self,
                        )?;
                    } else if type_condition.map_or(true, |condition| root.name() == condition) {
                        // The fragment applies to an interface type.
                        let _ctx = ctx.with_selection_set(selection_set);
                        // self.add_set(&ctx, root)?;
                        todo!()
                    }
                }
            }
        }
        Ok(())
    }

    /// Add another set of fields to this set of fields using the given container.
    /// Native way of resolving
    pub fn add_set_native<T: ContainerType + ?Sized>(
        &mut self,
        ctx: &ContextSelectionSet<'a>,
        root: &'a T,
    ) -> ServerResult<()> {
        for selection in &ctx.item.node.items {
            match &selection.node {
                Selection::Field(field) => {
                    if field.node.name.node == "__typename" {
                        // Get the typename
                        let ctx_field = ctx.with_field(field, None, Some(&ctx.item.node));
                        let field_name = ctx_field.item.node.response_key().node.clone();
                        let typename = root.introspection_type_name().into_owned();

                        self.0.push(Box::pin(async move {
                            Ok((field_name, Value::String(typename)))
                        }));
                        continue;
                    }

                    let resolve_fut = Box::pin({
                        let ctx = ctx.clone();
                        async move {
                            let ctx_field = ctx.with_field(field, None, Some(&ctx.item.node));
                            let field_name = ctx_field.item.node.response_key().node.clone();
                            let extensions = &ctx.query_env.extensions;

                            if extensions.is_empty() && field.node.directives.is_empty() {
                                Ok((
                                    field_name,
                                    root.resolve_field(&ctx_field).await?.unwrap_or_default(),
                                ))
                            } else {
                                let type_name = T::type_name();
                                let resolve_info = ResolveInfo {
                                    path_node: ctx_field.path_node.as_ref().unwrap(),
                                    parent_type: &type_name,
                                    return_type: match ctx_field
                                        .schema_env
                                        .registry
                                        .types
                                        .get(type_name.as_ref())
                                        .and_then(|ty| {
                                            ty.field_by_name(field.node.name.node.as_str())
                                        })
                                        .map(|field| &field.ty)
                                    {
                                        Some(ty) => &ty,
                                        None => {
                                            return Err(ServerError::new(
                                                format!(
                                                    r#"Cannot query field "{}" on type "{}"."#,
                                                    field_name, type_name
                                                ),
                                                Some(ctx_field.item.pos),
                                            ));
                                        }
                                    },
                                    name: field.node.name.node.as_str(),
                                    alias: field
                                        .node
                                        .alias
                                        .as_ref()
                                        .map(|alias| alias.node.as_str()),
                                };

                                let resolve_fut = root.resolve_field(&ctx_field);

                                if field.node.directives.is_empty() {
                                    futures_util::pin_mut!(resolve_fut);
                                    Ok((
                                        field_name,
                                        extensions
                                            .resolve(resolve_info, &mut resolve_fut)
                                            .await?
                                            .unwrap_or_default(),
                                    ))
                                } else {
                                    let mut resolve_fut = resolve_fut.boxed();

                                    for directive in &field.node.directives {
                                        if let Some(directive_factory) = ctx
                                            .schema_env
                                            .custom_directives
                                            .get(directive.node.name.node.as_str())
                                        {
                                            let ctx_directive = ContextBase {
                                                path_node: ctx_field.path_node,
                                                resolver_node: ctx_field.resolver_node.clone(),
                                                item: directive,
                                                schema_env: ctx_field.schema_env,
                                                query_env: ctx_field.query_env,
                                                resolvers_cache: ctx_field.resolvers_cache.clone(),
                                                resolvers_data: ctx_field.resolvers_data.clone(),
                                            };
                                            let directive_instance = directive_factory
                                                .create(&ctx_directive, &directive.node)?;
                                            resolve_fut = Box::pin({
                                                let ctx_field = ctx_field.clone();
                                                async move {
                                                    directive_instance
                                                        .resolve_field(&ctx_field, &mut resolve_fut)
                                                        .await
                                                }
                                            });
                                        }
                                    }

                                    Ok((
                                        field_name,
                                        extensions
                                            .resolve(resolve_info, &mut resolve_fut)
                                            .await?
                                            .unwrap_or_default(),
                                    ))
                                }
                            }
                        }
                    });

                    self.0.push(resolve_fut);
                }
                selection => {
                    let (type_condition, selection_set) = match selection {
                        Selection::Field(_) => unreachable!(),
                        Selection::FragmentSpread(spread) => {
                            let fragment =
                                ctx.query_env.fragments.get(&spread.node.fragment_name.node);
                            let fragment = match fragment {
                                Some(fragment) => fragment,
                                None => {
                                    return Err(ServerError::new(
                                        format!(
                                            r#"Unknown fragment "{}"."#,
                                            spread.node.fragment_name.node
                                        ),
                                        Some(spread.pos),
                                    ));
                                }
                            };
                            (
                                Some(&fragment.node.type_condition),
                                &fragment.node.selection_set,
                            )
                        }
                        Selection::InlineFragment(fragment) => (
                            fragment.node.type_condition.as_ref(),
                            &fragment.node.selection_set,
                        ),
                    };
                    let type_condition =
                        type_condition.map(|condition| condition.node.on.node.as_str());

                    let introspection_type_name = root.introspection_type_name();

                    let applies_concrete_object = type_condition.map_or(false, |condition| {
                        introspection_type_name == condition
                            || ctx
                                .schema_env
                                .registry
                                .implements
                                .get(&*introspection_type_name)
                                .map_or(false, |interfaces| interfaces.contains(condition))
                    });
                    if applies_concrete_object {
                        root.collect_all_fields_native(
                            &ctx.with_selection_set(selection_set),
                            self,
                        )?;
                    } else if type_condition.map_or(true, |condition| T::type_name() == condition) {
                        // The fragment applies to an interface type.
                        self.add_set_native(&ctx.with_selection_set(selection_set), root)?;
                    }
                }
            }
        }
        Ok(())
    }
}
