use std::any::Any;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use async_lock::RwLock;
use cached::UnboundCache;

use dynamodb::CurrentDateTime;
use futures_util::stream::{self, Stream, StreamExt};
use graph_entities::QueryResponse;
use indexmap::map::IndexMap;

use crate::context::{Data, QueryEnvInner};
use crate::custom_directive::CustomDirectiveFactory;
use crate::extensions::{ExtensionFactory, Extensions};
#[cfg(feature = "query-planning")]
use crate::logical_plan_utils::resolve_logical_plan_container;
use crate::model::__DirectiveLocation;
use crate::parser::types::{Directive, DocumentOperations, OperationType, Selection, SelectionSet};
use crate::parser::{parse_query, Positioned};
use crate::registry::{MetaDirective, MetaInputValue, Registry};
use crate::resolver_utils::{resolve_container, resolve_container_serial};
use crate::subscription::collect_subscription_streams;
use crate::types::QueryRoot;
use crate::validation::{check_rules, ValidationMode};
use crate::{
    BatchRequest, BatchResponse, CacheControl, ContextBase, InputType, ObjectType, OutputType,
    QueryEnv, Request, Response, ServerError, SubscriptionType, Variables, ID,
};
#[cfg(feature = "query-planning")]
use query_planning::execution_query::context::ExecutionContext;

#[cfg(feature = "query-planning")]
use query_planning::logical_query::{LogicalQuery, QueryOperationDefinition};

/// Schema builder
pub struct SchemaBuilder {
    validation_mode: ValidationMode,
    registry: Registry,
    data: Data,
    complexity: Option<usize>,
    depth: Option<usize>,
    extensions: Vec<Box<dyn ExtensionFactory>>,
    custom_directives: HashMap<&'static str, Box<dyn CustomDirectiveFactory>>,
}

impl SchemaBuilder {
    /// Manually register a input type in the schema.
    ///
    /// You can use this function to register schema types that are not directly referenced.
    #[must_use]
    pub fn register_input_type<T: InputType>(mut self) -> Self {
        T::create_type_info(&mut self.registry);
        self
    }

    /// Manually register a output type in the schema.
    ///
    /// You can use this function to register schema types that are not directly referenced.
    #[must_use]
    pub fn register_output_type<T: OutputType>(mut self) -> Self {
        T::create_type_info(&mut self.registry);
        self
    }

    /// Disable introspection queries.
    #[must_use]
    pub fn disable_introspection(mut self) -> Self {
        self.registry.disable_introspection = true;
        self
    }

    /// Set the maximum complexity a query can have. By default, there is no limit.
    #[must_use]
    pub fn limit_complexity(mut self, complexity: usize) -> Self {
        self.complexity = Some(complexity);
        self
    }

    /// Set the maximum depth a query can have. By default, there is no limit.
    #[must_use]
    pub fn limit_depth(mut self, depth: usize) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Add an extension to the schema.
    ///
    /// # Examples
    ///
    /// ```rust, ignore
    /// use dynaql::*;
    ///
    /// struct Query;
    ///
    /// #[Object]
    /// impl Query {
    ///     async fn value(&self) -> i32 {
    ///         100
    ///     }
    /// }
    ///
    /// let schema = Schema::build(Query, EmptyMutation,EmptySubscription)
    ///     .extension(extensions::Logger)
    ///     .finish();
    /// ```
    #[must_use]
    pub fn extension(mut self, extension: impl ExtensionFactory) -> Self {
        self.extensions.push(Box::new(extension));
        self
    }

    /// Add a global data that can be accessed in the `Schema`. You access it with `Context::data`.
    #[must_use]
    pub fn data<D: Any + Send + Sync>(mut self, data: D) -> Self {
        self.data.insert(data);
        self
    }

    /// Set the validation mode, default is `ValidationMode::Strict`.
    #[must_use]
    pub fn validation_mode(mut self, validation_mode: ValidationMode) -> Self {
        self.validation_mode = validation_mode;
        self
    }

    /// Enable federation, which is automatically enabled if the Query has least one entity definition.
    #[must_use]
    pub fn enable_federation(mut self) -> Self {
        self.registry.enable_federation = true;
        self
    }

    /// Make the Federation SDL include subscriptions.
    ///
    /// Note: Not included by default, in order to be compatible with Apollo Server.
    #[must_use]
    pub fn enable_subscription_in_federation(mut self) -> Self {
        self.registry.federation_subscription = true;
        self
    }

    /// Override the name of the specified input type.
    #[must_use]
    pub fn override_input_type_description<T: InputType>(mut self, desc: &'static str) -> Self {
        self.registry.set_description(&T::type_name(), desc);
        self
    }

    /// Override the name of the specified output type.
    #[must_use]
    pub fn override_output_type_description<T: OutputType>(mut self, desc: &'static str) -> Self {
        self.registry.set_description(&T::type_name(), desc);
        self
    }

    /// Register a custom directive.
    ///
    /// # Panics
    ///
    /// Panics if the directive with the same name is already registered.
    #[must_use]
    pub fn directive<T: CustomDirectiveFactory>(mut self, directive: T) -> Self {
        let name = directive.name();
        let instance = Box::new(directive);

        instance.register(&mut self.registry);

        if name == "skip"
            || name == "include"
            || self.custom_directives.insert(name, instance).is_some()
        {
            panic!("Directive `{name}` already exists");
        }

        self
    }

    /// Build schema.
    pub fn finish(mut self) -> Schema {
        // federation
        if self.registry.enable_federation || self.registry.has_entities() {
            self.registry.create_federation_types();
        }

        Schema(Arc::new(SchemaInner {
            validation_mode: self.validation_mode,
            complexity: self.complexity,
            depth: self.depth,
            extensions: self.extensions,
            env: SchemaEnv(Arc::new(SchemaEnvInner {
                registry: self.registry,
                data: self.data,
                custom_directives: self.custom_directives,
            })),
        }))
    }
}

#[doc(hidden)]
pub struct SchemaEnvInner {
    pub registry: Registry,
    pub data: Data,
    pub custom_directives: HashMap<&'static str, Box<dyn CustomDirectiveFactory>>,
}

#[doc(hidden)]
#[derive(Clone)]
pub struct SchemaEnv(Arc<SchemaEnvInner>);

impl Deref for SchemaEnv {
    type Target = SchemaEnvInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[doc(hidden)]
pub struct SchemaInner {
    pub(crate) validation_mode: ValidationMode,
    pub(crate) complexity: Option<usize>,
    pub(crate) depth: Option<usize>,
    pub(crate) extensions: Vec<Box<dyn ExtensionFactory>>,
    pub(crate) env: SchemaEnv,
}

/// GraphQL schema.
///
/// Cloning a schema is cheap, so it can be easily shared.
pub struct Schema(Arc<SchemaInner>);

impl Clone for Schema {
    fn clone(&self) -> Self {
        Schema(self.0.clone())
    }
}

impl Default for Schema {
    fn default() -> Self {
        Schema::new(Self::create_registry())
    }
}

impl Deref for Schema {
    type Target = SchemaInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Schema {
    /// Create a schema builder
    ///
    /// The root object for the query and Mutation needs to be specified.
    /// If there is no mutation, you can use `EmptyMutation`.
    /// If there is no subscription, you can use `EmptySubscription`.
    pub fn build(registry: Registry) -> SchemaBuilder {
        SchemaBuilder {
            validation_mode: ValidationMode::Strict,
            registry,
            data: Default::default(),
            complexity: None,
            depth: None,
            extensions: Default::default(),
            custom_directives: Default::default(),
        }
    }

    pub fn create_registry_static<Query, Mutation, Subscription>() -> Registry
    where
        Query: ObjectType + 'static,
        Mutation: ObjectType + 'static,
        Subscription: SubscriptionType + 'static,
    {
        let mut registry = Registry {
            types: Default::default(),
            directives: Default::default(),
            implements: Default::default(),
            query_type: Query::type_name().to_string(),
            mutation_type: if Mutation::is_empty() {
                None
            } else {
                Some(Mutation::type_name().to_string())
            },
            subscription_type: if Subscription::is_empty() {
                None
            } else {
                Some(Subscription::type_name().to_string())
            },
            disable_introspection: false,
            enable_federation: false,
            federation_subscription: false,
            ..Default::default()
        };

        Schema::add_builtins_to_registry(&mut registry);

        QueryRoot::<Query>::create_type_info(&mut registry);
        if !Mutation::is_empty() {
            Mutation::create_type_info(&mut registry);
        }
        if !Subscription::is_empty() {
            Subscription::create_type_info(&mut registry);
        }

        registry.remove_unused_types();
        registry
    }

    pub fn create_registry() -> Registry {
        let mut registry = Registry {
            types: Default::default(),
            directives: Default::default(),
            implements: Default::default(),
            query_type: "Query".to_string(),
            mutation_type: None,
            subscription_type: None,
            disable_introspection: false,
            enable_federation: false,
            federation_subscription: false,
            ..Default::default()
        };

        Schema::add_builtins_to_registry(&mut registry);

        registry.remove_unused_types();
        registry
    }

    fn add_builtins_to_registry(registry: &mut Registry) {
        registry.add_directive(MetaDirective {
            name: "include".to_string(),
            description: Some("Directs the executor to include this field or fragment only when the `if` argument is true.".to_string()),
            locations: vec![
                __DirectiveLocation::FIELD,
                __DirectiveLocation::FRAGMENT_SPREAD,
                __DirectiveLocation::INLINE_FRAGMENT
            ],
            args: {
                let mut args = IndexMap::new();
                args.insert(
                    "if".to_string(),
                    MetaInputValue::new("if".to_string(), "Boolean!")
                        .with_description("Included when true.")
                );
                args
            },
            is_repeatable: false,
            visible: None,
        });

        registry.add_directive(MetaDirective {
            name: "skip".to_string(),
            description: Some("Directs the executor to skip this field or fragment when the `if` argument is true.".to_string()),
            locations: vec![
                __DirectiveLocation::FIELD,
                __DirectiveLocation::FRAGMENT_SPREAD,
                __DirectiveLocation::INLINE_FRAGMENT
            ],
            args: {
                let mut args = IndexMap::new();
                args.insert(
                    "if".to_string(),
                    MetaInputValue::new( "if", "Boolean!")
                        .with_description("Skipped when true."));
                args
            },
            is_repeatable: false,
            visible: None,
        });

        registry.add_directive(MetaDirective {
            name: "oneOf".to_string(),
            description: Some("Indicates that an input object is a oneOf input object".to_string()),
            locations: vec![__DirectiveLocation::INPUT_OBJECT],
            args: IndexMap::new(),
            is_repeatable: false,
            visible: Some(|_| true),
        });

        registry.add_directive(MetaDirective {
            name: "live".to_string(),
            description: Some(
                "Directs the executor to return values as a Streaming response.".to_string(),
            ),
            locations: vec![__DirectiveLocation::QUERY],
            args: { IndexMap::new() },
            is_repeatable: false,
            visible: None,
        });

        // register scalars
        <bool as InputType>::create_type_info(registry);
        <i32 as InputType>::create_type_info(registry);
        <f32 as InputType>::create_type_info(registry);
        <String as InputType>::create_type_info(registry);
        <ID as InputType>::create_type_info(registry);
    }

    /// Create a schema
    pub fn new(registry: Registry) -> Schema {
        Self::build(registry).finish()
    }

    #[inline]
    #[allow(unused)]
    pub fn registry(&self) -> &Registry {
        &self.env.registry
    }

    /// Returns SDL(Schema Definition Language) of this schema.
    pub fn sdl(&self) -> String {
        self.0.env.registry.export_sdl(false)
    }

    /// Returns Federation SDL(Schema Definition Language) of this schema.
    pub fn federation_sdl(&self) -> String {
        self.0.env.registry.export_sdl(true)
    }

    /// Get all names in this schema
    ///
    /// Maybe you want to serialize a custom binary protocol. In order to minimize message size, a dictionary
    /// is usually used to compress type names, field names, directive names, and parameter names. This function gets all the names,
    /// so you can create this dictionary.
    pub fn names(&self) -> Vec<String> {
        self.0.env.registry.names()
    }

    fn create_extensions(&self, session_data: Arc<Data>) -> Extensions {
        Extensions::new(
            self.extensions.iter().map(|f| f.create()),
            self.env.clone(),
            session_data,
        )
    }

    async fn prepare_request(
        &self,
        mut extensions: Extensions,
        request: Request,
        session_data: Arc<Data>,
    ) -> Result<(QueryEnv, CacheControl), Vec<ServerError>> {
        let mut request = request;
        let query_data = Arc::new(std::mem::take(&mut request.data));
        extensions.attach_query_data(query_data.clone());

        let request = extensions.prepare_request(request).await?;
        let mut document = {
            let query = &request.query;
            let fut_parse = async { parse_query(&query).map_err(Into::<ServerError>::into) };
            futures_util::pin_mut!(fut_parse);
            extensions
                .parse_query(&query, &request.variables, &mut fut_parse)
                .await?
        };

        // check rules
        let validation_result = {
            let validation_fut = async {
                check_rules(
                    &self.env.registry,
                    &document,
                    Some(&request.variables),
                    self.validation_mode,
                )
            };
            futures_util::pin_mut!(validation_fut);
            extensions.validation(&mut validation_fut).await?
        };

        // check limit
        if let Some(limit_complexity) = self.complexity {
            if validation_result.complexity > limit_complexity {
                return Err(vec![ServerError::new("Query is too complex.", None)]);
            }
        }

        if let Some(limit_depth) = self.depth {
            if validation_result.depth > limit_depth {
                return Err(vec![ServerError::new("Query is nested too deep.", None)]);
            }
        }

        let operation = if let Some(operation_name) = &request.operation_name {
            match document.operations {
                DocumentOperations::Single(_) => None,
                DocumentOperations::Multiple(mut operations) => operations
                    .remove(operation_name.as_str())
                    .map(|operation| (Some(operation_name.clone()), operation)),
            }
            .ok_or_else(|| {
                ServerError::new(
                    format!(r#"Unknown operation named "{operation_name}""#),
                    None,
                )
            })
        } else {
            match document.operations {
                DocumentOperations::Single(operation) => Ok((None, operation)),
                DocumentOperations::Multiple(map) if map.len() == 1 => {
                    let (operation_name, operation) = map.into_iter().next().unwrap();
                    Ok((Some(operation_name.to_string()), operation))
                }
                DocumentOperations::Multiple(_) => Err(ServerError::new(
                    "Operation name required in request.",
                    None,
                )),
            }
        };

        let (operation_name, mut operation) = operation.map_err(|err| vec![err])?;

        // remove skipped fields
        for fragment in document.fragments.values_mut() {
            remove_skipped_selection(&mut fragment.node.selection_set.node, &request.variables);
        }
        remove_skipped_selection(&mut operation.node.selection_set.node, &request.variables);

        // We could have the whole flow here to create the LogicalQuery
        // As the rules passed, we could in theory have a working LogicalQuery
        //
        // Then we can pass it along with other variables to an execution layer
        // Or just print it for now.
        // LogicalQuery::build(document, registry);

        // cache only queries
        let cache_control = match operation.node.ty {
            OperationType::Query => validation_result.cache_control,
            _ => CacheControl::default(),
        };

        let env = QueryEnvInner {
            extensions,
            variables: request.variables,
            operation_name,
            operation,
            fragments: document.fragments,
            uploads: request.uploads,
            session_data,
            ctx_data: query_data,
            response_http_headers: Default::default(),
            disable_introspection: request.disable_introspection,
            errors: Default::default(),
            current_datetime: CurrentDateTime::new(),
        };
        Ok((QueryEnv::new(env), cache_control))
    }

    async fn execute_once(&self, env: QueryEnv) -> Response {
        // execute
        let ctx = ContextBase {
            path_node: None,
            resolver_node: None,
            item: &env.operation.node.selection_set,
            schema_env: &self.env,
            query_env: &env,
            resolvers_cache: Arc::new(RwLock::new(UnboundCache::with_capacity(32))),
            resolvers_data: Default::default(),
            response_graph: Arc::new(RwLock::new(QueryResponse::default())),
        };

        let query = ctx.registry().query_root();

        let res = match &env.operation.node.ty {
            OperationType::Query => resolve_container(&ctx, query, None).await,
            OperationType::Mutation => {
                resolve_container_serial(&ctx, ctx.registry().mutation_root(), None).await
            }
            OperationType::Subscription => Err(ServerError::new(
                "Subscriptions are not supported on this transport.",
                None,
            )),
        };

        let mut resp = match res {
            Ok(value) => {
                let response = &mut *ctx.response_graph.write().await;
                response.set_root_unchecked(value);
                let a = QueryResponse::default();
                let b = std::mem::replace(response, a);
                Response::new(b)
            }
            Err(err) => Response::from_errors(vec![err]),
        }
        .http_headers(std::mem::take(
            &mut *env.response_http_headers.lock().unwrap(),
        ));

        resp.errors
            .extend(std::mem::take(&mut *env.errors.lock().unwrap()));
        resp
    }

    /// LogicalQuery execution mechanism, we'll visit each node with the context
    /// of the actual query and the schema, then it'll create the actual
    /// [`LogicalQuery`] to be execute or give back an error.
    ///
    /// Theorically there shouldn't have any errors as we validate the GraphQL
    /// query before executing it.
    #[cfg(feature = "query-planning")]
    #[allow(unused)]
    async fn logical_query_once(&self, env: QueryEnv) -> Response {
        use query_planning::logical_query::QueryOperations;

        let ctx = ContextBase {
            path_node: None,
            resolver_node: None,
            item: &env.operation.node.selection_set,
            schema_env: &self.env,
            query_env: &env,
            resolvers_cache: Arc::new(RwLock::new(UnboundCache::with_capacity(32))),
            resolvers_data: Default::default(),
            response_graph: Arc::new(RwLock::new(QueryResponse::default())),
        };

        let query = ctx.registry().query_root();

        let res =
            match &env.operation.node.ty {
                OperationType::Query => resolve_logical_plan_container(&ctx, query, None)
                    .await
                    .map(|x| QueryOperationDefinition {
                        ty: OperationType::Query,
                        selection_set: x,
                    }),
                OperationType::Mutation => {
                    resolve_logical_plan_container(&ctx, ctx.registry().mutation_root(), None)
                        .await
                        .map(|x| QueryOperationDefinition {
                            ty: OperationType::Mutation,
                            selection_set: x,
                        })
                }
                OperationType::Subscription => Err(ServerError::new(
                    "Subscriptions are not supported on this transport.",
                    None,
                )),
            };

        let mut resp = match res {
            Ok(value) => {
                let query = LogicalQuery {
                    operations: QueryOperations::Single(value),
                };
                return Response::from_logical_query(query);
            }
            Err(err) => Response::from_errors(vec![err]),
        };

        resp.errors
            .extend(std::mem::take(&mut *env.errors.lock().unwrap()));
        resp
    }

    /// LogicalQuery creation & Execution.
    #[cfg(feature = "query-planning")]
    async fn logical_query_execute_once(&self, env: QueryEnv) -> Response {
        use query_planning::execution_query::context::Context;
        use query_planning::execution_query::planner::PhysicalQueryPlanner;
        use query_planning::execution_query::ExecuteStream;
        use query_planning::logical_query::QueryOperations;

        let ctx = ContextBase {
            path_node: None,
            resolver_node: None,
            item: &env.operation.node.selection_set,
            schema_env: &self.env,
            query_env: &env,
            resolvers_cache: Arc::new(RwLock::new(UnboundCache::with_capacity(32))),
            resolvers_data: Default::default(),
            response_graph: Arc::new(RwLock::new(QueryResponse::default())),
        };

        #[cfg(feature = "query-planning")]
        let engine =
            ctx.data_unchecked::<grafbase_runtime::custom_resolvers::CustomResolversEngine>();

        #[cfg(feature = "query-planning")]
        let req = ctx.data_unchecked::<grafbase_runtime::GraphqlRequestExecutionContext>();

        #[cfg(feature = "query-planning")]
        let exec_context = ctx.data_unchecked::<Arc<ExecutionContext>>().clone();

        let query = ctx.registry().query_root();

        let res =
            match &env.operation.node.ty {
                OperationType::Query => resolve_logical_plan_container(&ctx, query, None)
                    .await
                    .map(|x| QueryOperationDefinition {
                        ty: OperationType::Query,
                        selection_set: x,
                    }),
                OperationType::Mutation => {
                    resolve_logical_plan_container(&ctx, ctx.registry().mutation_root(), None)
                        .await
                        .map(|x| QueryOperationDefinition {
                            ty: OperationType::Mutation,
                            selection_set: x,
                        })
                }
                OperationType::Subscription => Err(ServerError::new(
                    "Subscriptions are not supported on this transport.",
                    None,
                )),
            };

        let mut resp = match res {
            Ok(value) => {
                let query = LogicalQuery {
                    operations: QueryOperations::Single(value),
                };
                let planner = PhysicalQueryPlanner::default();
                let exec = planner.create_execution_query(&query).unwrap();
                let ctx = Context::new(exec_context);
                let execute_stream = exec.execute(&ctx).unwrap().fuse();
                let exec_future = execute_stream.into_future();
                let (first_result, _rest) = exec_future.await;

                let a = format!("{first_result:?}");

                #[cfg(feature = "tracing_worker")]
                logworker::info!("", "{a}",);

                // Some unwrap here, behind a feature flag, not an issue to crash the worker.
                let mut first_response: serde_json::Value =
                    first_result.unwrap().unwrap().to_json();

                let mut root = QueryResponse::default();
                let c = first_response.get_mut("data").unwrap().take();
                let id = root.from_serde_value(c);
                root.set_root_unchecked(id);

                return Response::new(root);
            }
            Err(err) => Response::from_errors(vec![err]),
        };

        resp.errors
            .extend(std::mem::take(&mut *env.errors.lock().unwrap()));
        resp
    }

    /// Execute a GraphQL query.
    pub async fn execute(&self, request: impl Into<Request>) -> Response {
        let request = request.into();
        #[cfg(feature = "query-planning")]
        let is_logical_plan = request.logic_plan;
        let extensions = self.create_extensions(Default::default());
        let request_fut = {
            let extensions = extensions.clone();
            async move {
                match self
                    .prepare_request(extensions, request, Default::default())
                    .await
                {
                    Ok((env, cache_control)) => {
                        // After the request is ready, we start the creation of the Logic
                        #[cfg(feature = "query-planning")]
                        {
                            if is_logical_plan {
                                return self.logical_query_execute_once(env.clone()).await;
                            }
                        }

                        let fut = async {
                            self.execute_once(env.clone())
                                .await
                                .cache_control(cache_control)
                        };
                        futures_util::pin_mut!(fut);
                        env.extensions
                            .execute(env.operation_name.as_deref(), &mut fut)
                            .await
                    }
                    Err(errors) => Response::from_errors(errors),
                }
            }
        };
        futures_util::pin_mut!(request_fut);
        extensions.request(&mut request_fut).await
    }

    /// Execute a GraphQL batch query.
    pub async fn execute_batch(&self, batch_request: BatchRequest) -> BatchResponse {
        match batch_request {
            BatchRequest::Single(request) => BatchResponse::Single(self.execute(request).await),
            BatchRequest::Batch(requests) => BatchResponse::Batch(
                futures_util::stream::iter(requests.into_iter())
                    .then(|request| self.execute(request))
                    .collect()
                    .await,
            ),
        }
    }

    /// Execute a GraphQL subscription with session data.
    #[doc(hidden)]
    pub fn execute_stream_with_session_data(
        &self,
        request: impl Into<Request> + Send,
        session_data: Arc<Data>,
    ) -> impl Stream<Item = Response> + Send + Unpin {
        let schema = self.clone();
        let request = request.into();
        let extensions = self.create_extensions(session_data.clone());

        let stream = futures_util::stream::StreamExt::boxed({
            let extensions = extensions.clone();
            async_stream::stream! {
                let (env, cache_control) = match schema.prepare_request(extensions, request, session_data).await {
                    Ok(res) => res,
                    Err(errors) => {
                        yield Response::from_errors(errors);
                        return;
                    }
                };

                if env.operation.node.ty != OperationType::Subscription {
                    yield schema.execute_once(env).await.cache_control(cache_control);
                    return;
                }

                let ctx = env.create_context(
                    &schema.env,
                    None,
                    None,
                    &env.operation.node.selection_set,
                );

                let mut streams = Vec::new();
                // if let Err(err) = collect_subscription_streams(&ctx, &schema.subscription, &mut streams) {
                if let Err(err) = collect_subscription_streams(&ctx, &crate::EmptySubscription, &mut streams) {
                    yield Response::from_errors(vec![err]);
                }

                let mut stream = stream::select_all(streams);
                while let Some(resp) = stream.next().await {
                    yield resp;
                }
            }
        });
        extensions.subscribe(stream)
    }

    /// Execute a GraphQL subscription.
    pub fn execute_stream(
        &self,
        _request: impl Into<Request>,
    ) -> impl Stream<Item = Response> + Send + Unpin {
        futures_util::stream::empty()
        // self.execute_stream_with_session_data(request.into(), Default::default())
    }
}

fn remove_skipped_selection(selection_set: &mut SelectionSet, variables: &Variables) {
    fn is_skipped(directives: &[Positioned<Directive>], variables: &Variables) -> bool {
        for directive in directives {
            let include = match &*directive.node.name.node {
                "skip" => false,
                "include" => true,
                _ => continue,
            };

            if let Some(condition_input) = directive.node.get_argument("if") {
                let value = condition_input
                    .node
                    .clone()
                    .into_const_with(|name| variables.get(&name).cloned().ok_or(()))
                    .unwrap_or_default();
                let value: bool = InputType::parse(Some(value)).unwrap_or_default();
                if include != value {
                    return true;
                }
            }
        }

        false
    }

    selection_set
        .items
        .retain(|selection| !is_skipped(selection.node.directives(), variables));

    for selection in &mut selection_set.items {
        selection.node.directives_mut().retain(|directive| {
            directive.node.name.node != "skip" && directive.node.name.node != "include"
        });
    }

    for selection in &mut selection_set.items {
        match &mut selection.node {
            Selection::Field(field) => {
                remove_skipped_selection(&mut field.node.selection_set.node, variables);
            }
            Selection::FragmentSpread(_) => {}
            Selection::InlineFragment(inline_fragment) => {
                remove_skipped_selection(&mut inline_fragment.node.selection_set.node, variables);
            }
        }
    }
}
