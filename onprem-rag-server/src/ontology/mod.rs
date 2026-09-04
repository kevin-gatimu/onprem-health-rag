//! Service-line ontology and schema binding.
//!
//! This module provides:
//!
//! - [`service_line`] — `ServiceLine` enum (13 tiers), labels, vocabulary, concept
//!   ownership, and example questions.
//! - [`concepts`] — `EntityConcept` enum (~64 concepts + Unknown), descriptors, and
//!   `SHARED_CONCEPTS`.
//! - [`roles`] — `ColumnRole` enum (35 variants), token patterns, and PII/categorical
//!   flags.
//! - [`binding`] — `ColumnBinding`, `TableBinding`, `SchemaBinding`, `BindingCoverage`.
//! - [`binder`] — `bind_cards` (sync, pure) and `build_binding` (async).
//! - [`store`] — DocumentDB persistence for `SchemaBinding`.
//! - [`routes`] — HTTP endpoints (`/agents`, `/sources/<id>/binding`, …).

pub mod binding;
pub mod binder;
pub mod concepts;
pub mod roles;
pub mod routes;
pub mod service_line;
pub mod store;

#[cfg(test)]
pub mod tests;

pub use binding::{BindingCoverage, ColumnBinding, JoinHop, SchemaBinding, TableBinding};
pub use binder::{BindingOverrides, bind_cards, build_binding, compute_descriptor_vectors};
pub use concepts::{ANCHOR_CONCEPTS, EntityConcept, SHARED_CONCEPTS, descriptor};
pub use roles::{ColumnRole, ROLE_TOKENS, RoleTokens, TypeClass};
pub use service_line::ServiceLine;
