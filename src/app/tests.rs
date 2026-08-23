//! Unit tests for the `app` module.
//!
//! One module per surface, mirroring the `app/` submodule split. The
//! bodies moved verbatim out of a single 9,515-line file; `use
//! super::super::*` in each resolves to `crate::app`, exactly what the
//! flat file's `use super::*` gave them, so every test still sees what
//! it did when it lived inline.
//!
//! Shared fixtures live in `support` and are `pub(super)`, which makes
//! them visible to every sibling here.

mod audit;
mod cost;
mod detail;
mod dispatch;
mod dlq;
mod formatting;
mod keys;
mod lint;
mod overlays;
mod parsing;
mod pure;
mod refresh;
mod region;
mod render;
mod safety;
mod support;
