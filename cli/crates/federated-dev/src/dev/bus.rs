mod admin;
mod compose;
mod message;
mod refresh;

pub(crate) use admin::AdminBus;
pub(crate) use compose::ComposeBus;
pub(crate) use message::*;
pub(crate) use refresh::RefreshBus;

use crate::{dev::composer::Subgraph, error::Error};
use async_graphql_parser::types::ServiceDocument;
use graphql_composition::FederatedGraph;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use super::{admin::Header, refresher::RefreshMessage};

/// A channel to send composed federated graph, typically to a router.
pub(crate) type GraphSender = mpsc::Sender<FederatedGraph>;

/// A channel to receive a composed federated graph, typically for a router.
pub(crate) type GraphReceiver = mpsc::Receiver<FederatedGraph>;

/// A channel to send a refresh message with a collection of graphs.
pub(crate) type RefreshSender = mpsc::Sender<Vec<RefreshMessage>>;

/// A channel to receive a refresh message with a collection of graphs.
pub(crate) type RefreshReceiver = mpsc::Receiver<Vec<RefreshMessage>>;

/// Send channel for the composer to control its state and actions
pub(crate) type ComposeSender = mpsc::Sender<ComposeMessage>;

/// Receive channel for the composer to control its state and actions
pub(crate) type ComposeReceiver = mpsc::Receiver<ComposeMessage>;

async fn compose_graph(
    sender: &ComposeSender,
    name: String,
    url: Url,
    headers: Vec<Header>,
    schema: ServiceDocument,
) -> Result<(), Error> {
    let (request, response) = oneshot::channel();
    let subgraph = Subgraph::new(url, headers, schema);

    let message = ComposeSchema::new(name, subgraph, request);
    sender.send(message.into()).await?;

    response.await?
}

async fn introspect_schema(
    sender: &ComposeSender,
    name: &str,
    url: Url,
    headers: Vec<Header>,
) -> Result<ServiceDocument, Error> {
    let (request, response) = oneshot::channel();
    let message = IntrospectSchema::new(name, url, request, headers);

    sender
        .send(ComposeMessage::Introspect(message))
        .await
        .map_err(|error| Error::internal(error.to_string()))?;

    response.await?
}