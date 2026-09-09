//! Pure decision logic: graph + config → a list of engine commands.
//!
//! Everything in here is a **function of its arguments**. No threads, no
//! `PipeWire`, no filesystem, no clock. That is the whole point: the
//! logic that decides how a studio gets wired is the part where a bug
//! silently mis-routes a live production graph, so it must be testable
//! without any of that machinery.
//!
//! The shape is always the same:
//!
//! ```text
//! plan(&GraphStore, &config) -> Vec<Command>     // pure, tested here
//! for cmd in plan { engine.send(cmd) }           // thin I/O, in service.rs
//! ```
//!
//! A planner never mutates the store and never talks to the engine, so
//! a test builds a `GraphStore` by hand and asserts on the commands that
//! come back. Planners are also idempotent: run one against the graph
//! its own commands produced and it returns an empty plan.

pub(crate) mod cycles;
pub(crate) mod pairing;
pub(crate) mod presets;
pub(crate) mod routes;
pub(crate) mod sinks;

#[cfg(test)]
pub(crate) mod fixtures;
