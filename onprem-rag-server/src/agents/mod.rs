//! Hospital agents: the 13 service lines plus the router-driven **Ask** entry
//! point, exposed as `POST /agents/<kind>` with a roster at `GET /agents`.
//!
//! - `kind`     — product identity (`AgentKind`, `AgentMode`) and the read
//!                allow-list each agent gets. Not an authorisation boundary.
//! - `persona`  — system prompts and capability answers generated from the
//!                deployment's own schema binding.
//! - `registry` — `GET /agents`, the roster the UI renders.
//! - `routes`   — the single shared request flow.

pub mod kind;
pub mod persona;
pub mod registry;
pub mod routes;
