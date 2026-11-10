// `#[async_trait]` expands every trait method to a `#[must_use]` fn returning
// a boxed future, which newer clippy (`double_must_use`, seen on 1.99) flags on
// every async trait in the crate — upstream GrokSearch-rs code included. The
// expansion is the macro's, not ours, so the lint is allowed crate-wide.
#![allow(clippy::double_must_use)]

pub mod adapters;
pub mod cache;
pub mod clock;
pub mod config;
pub mod credentials;
pub mod error;
#[cfg(feature = "http")]
pub mod http;
pub mod logging;
pub mod mcp;
pub mod model;
pub mod oauth;
pub mod providers;
pub mod redact;
pub mod service;
pub mod sources;
