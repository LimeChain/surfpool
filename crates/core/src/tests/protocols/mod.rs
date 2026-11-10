//! Tests against live mainnet state, one module per integrated protocol.
//!
//! They need a network, so they are compiled only behind a feature. One filter runs them all:
//!
//! ```text
//! cargo test -p surfpool-core --features integration-tests tests::protocols::
//! ```
//!
//! A protocol's module is declared here and nowhere else; the filter above then covers it.

pub mod kamino;
pub mod phoenix;
pub mod pump;
