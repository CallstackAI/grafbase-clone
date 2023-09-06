#![allow(unused_crate_dependencies, clippy::panic)]

mod engine;
pub mod helpers;
pub mod mocks;
pub mod mongodb;
pub mod types;
pub mod udfs;

pub use engine::{Engine, EngineBuilder};
pub use helpers::{GetPath, ResponseExt};
pub use mocks::MockConnectorParsers;
pub use mongodb::{with_mongodb, with_namespaced_mongodb};
pub use types::{Error, ResponseData};

use names::{Generator, Name};
use std::{cell::RefCell, sync::OnceLock};
use tokio::runtime::Runtime;

thread_local! {
    static NAMES: RefCell<Option<Generator<'static>>> = RefCell::new(None);
}

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().unwrap())
}

fn random_name() -> String {
    NAMES.with(|maybe_generator| {
        maybe_generator
            .borrow_mut()
            .get_or_insert_with(|| Generator::with_naming(Name::Plain))
            .next()
            .unwrap()
            .replace('-', "")
    })
}