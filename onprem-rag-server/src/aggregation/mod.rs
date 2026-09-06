//! Aggregation-aware retrieval: from a natural-language analytical question to
//! exact DocumentDB aggregation results with grounded narration.
//!
//! Sub-modules:
//! - `spec`     — `RunAggregation`, `RunList`, and their helper types
//! - `catalog`  — dynamic metadata catalog (allow-list + planner grounding)
//! - `validate` — safety validation (field/collection allow-list, operator block, cap)
//! - `execute`  — aggregation pipeline builder + async runner (`AggRow` result shape)
//! - `list`     — paginated list executor + `validate_list`
//! - `intent`   — lexical `QueryIntent` classifier
//! - `from_ir`  — deterministic `QuerySpec` (IR) -> `RunAggregation`/`RunList`

pub mod catalog;
pub mod execute;
pub mod from_ir;
pub mod intent;
pub mod list;
pub mod spec;
pub mod validate;

// Convenience re-exports.
#[allow(unused_imports)]
pub use execute::AggRow;
#[allow(unused_imports)]
pub use intent::QueryIntent;
pub use spec::RunAggregation;
pub use validate::validate;
