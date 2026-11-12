pub mod prompts;
pub mod resources;
pub mod revisions;
pub mod sampling;
pub mod sdk;
pub mod setup;
pub mod telemetry;
pub mod tools;
pub mod transport;
pub mod types;

// Shared by the setup, prompts and tool-schema tests that keep numeric rewrite
// goals out of the text an agent reads.
#[cfg(test)]
#[path = "../../tests/unit/mcp/rewrite_goal.rs"]
pub(crate) mod rewrite_goal;
